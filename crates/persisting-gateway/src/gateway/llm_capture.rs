//! LLM request/response capture handler (non-streaming path + upstream orchestration).

use std::sync::Arc;

use anyhow::Context;
use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use persisting_agentctl::{ModelCallRequest, RunId, StorylineId};
use serde_json::Value;

use super::auth::{apply_upstream_headers, resolve_upstream_api_key};
use super::common::{
    attach_capture_headers, call_context, extract_model, is_models_list_path, model_access_policy,
};
use super::models_list::build_models_response;
use super::router::resolve_route;
use super::state::GatewayState;
use super::streaming::{should_stream_to_client, streaming_llm_response};
use super::upstream::prepare_upstream_body;
use crate::config::ProxyConfig;
use crate::conversion::{
    translate_error_for_bridge, translate_response_for_bridge, ProtocolBridge,
    MAX_REQUEST_BODY_BYTES, MAX_RESPONSE_BODY_BYTES,
};
use crate::engine::headers_to_vec;
use crate::engine::{CompleteEvent, Event, RequestEvent};
use crate::protocol::ProtocolKind;
use crate::runtime::debug::{self, truncate_body_bytes};
use crate::session::storage::resolve_capture_route;
use crate::understanding::understand_request;
use crate::Call;
use persisting_overlaynet::headers::{
    is_websocket_upgrade, skip_response_header_when_body_changed,
};

fn client_request_url(parts: &axum::http::request::Parts) -> String {
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(parts.uri.path());
    if let Some(host) = parts
        .headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        // Scheme is not visible on the proxy request; record as authority+path for correlation.
        format!("//{host}{path_and_query}")
    } else {
        path_and_query.to_string()
    }
}

fn http_version_label(version: axum::http::Version) -> String {
    match version {
        axum::http::Version::HTTP_09 => "HTTP/0.9".into(),
        axum::http::Version::HTTP_10 => "HTTP/1.0".into(),
        axum::http::Version::HTTP_11 => "HTTP/1.1".into(),
        axum::http::Version::HTTP_2 => "HTTP/2".into(),
        axum::http::Version::HTTP_3 => "HTTP/3".into(),
        other => format!("{other:?}"),
    }
}

pub(super) async fn llm_capture(
    state: GatewayState,
    req: Request,
    peer: std::net::SocketAddr,
    cfg: Arc<ProxyConfig>,
    debug_on: bool,
) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();

    if is_websocket_upgrade(&parts.headers) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from(
                "WebSocket transport is not supported by the capture gateway; use HTTPS",
            ))
            .map_err(|e| anyhow::anyhow!("websocket rejection response: {e}"))?
            .into_response());
    }

    let body_bytes = match to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(
                target: "persisting_gateway",
                %error,
                limit = MAX_REQUEST_BODY_BYTES,
                "rejecting oversized LLM request body"
            );
            return Ok((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("LLM request body exceeds {MAX_REQUEST_BODY_BYTES} bytes"),
            )
                .into_response());
        }
    };
    let path = parts.uri.path().to_string();
    let method = parts.method.clone();
    let protocol = ProtocolKind::from_path(&path);

    let capture_route = resolve_capture_route(
        &parts.headers,
        &body_bytes,
        &state.config.session_header,
        state.storage.as_path(),
    );
    let call = Call::from_headers(&parts.headers);
    let agent_id = cfg.agent_id.clone();
    let session_id = capture_route.session_id.clone();

    if method == Method::GET && is_models_list_path(&path) {
        let json = build_models_response(&cfg);
        let bytes = serde_json::to_vec(&json).context("serialize /v1/models")?;
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .context("build /v1/models response")?
            .into_response());
    }

    let client_meta =
        state
            .session_clients
            .ensure(state.storage.as_path(), &agent_id, &capture_route, peer);

    // Understand the untouched client request once. Routing, rendering, and live capture below
    // share this typed value; WAL retains the pre-rewrite JSON for crash replay.
    let mut parsed_request = understand_request(protocol, &body_bytes).ok();
    let stream_request = parsed_request
        .as_ref()
        .map(|parsed| parsed.semantic.request.stream)
        .unwrap_or_else(|| super::streaming::request_wants_stream(&body_bytes))
        || path.ends_with(":streamGenerateContent");
    let client_model = parsed_request
        .as_ref()
        .and_then(|parsed| parsed.semantic.request.model.clone())
        .or_else(|| extract_model(&body_bytes))
        .or_else(|| gemini_model_from_path(&path))
        .unwrap_or_else(|| "_unknown".to_string());
    if let Some(parsed) = parsed_request.as_mut() {
        let semantic = Arc::make_mut(&mut parsed.semantic);
        semantic
            .request
            .model
            .get_or_insert_with(|| client_model.clone());
        semantic.request.stream = stream_request;
    }
    let resolved = resolve_route(&cfg.models, &client_model)?;
    let route = resolved.route;
    let upstream_model = resolved.upstream_model.clone();
    let bridge = ProtocolBridge::needed(protocol, route);
    let upstream_protocol = bridge.upstream_protocol(protocol);
    let provider = route.effective_provider(upstream_protocol);

    // Wrap once in Arc so the request, response (or stream draft), and final events
    // all share a single allocation; clones become refcount bumps.
    let mut ctx = call_context(
        &capture_route,
        &agent_id,
        &call,
        &parts.headers,
        &client_model,
        &upstream_model,
        provider,
        protocol,
        cfg.capture_level,
        debug_on,
    );
    ctx.attach_client(peer, client_meta);
    ctx.attach_http_version(http_version_label(parts.version));
    let mut call_ctx: Arc<_> = Arc::new(ctx);

    {
        let user_content = parsed_request
            .as_ref()
            .and_then(|parsed| parsed.latest_visible_user_content.clone());
        let body_json = parsed_request
            .as_ref()
            .map(|parsed| parsed.body_json.clone())
            .or_else(|| serde_json::from_slice::<Value>(&body_bytes).ok());
        let semantic = parsed_request
            .as_ref()
            .map(|parsed| Arc::clone(&parsed.semantic));
        state.capture_engine.spawn_apply(
            Arc::clone(&call_ctx),
            Event::Request(RequestEvent {
                path: path.clone(),
                method: method.as_str().to_string(),
                url: Some(client_request_url(&parts)),
                body_bytes: body_bytes.len(),
                user_content,
                body_json,
                semantic,
                model_rewritten: resolved.model_rewritten,
                headers: headers_to_vec(&parts.headers),
            }),
        );
    }

    let upstream_body = prepare_upstream_body(
        &body_bytes,
        parsed_request
            .as_ref()
            .map(|parsed| parsed.semantic.as_ref()),
        resolved.model_rewritten,
        &upstream_model,
        bridge,
        Some(state.reasoning_cache.as_ref()),
    )?;

    let upstream_path = bridge.upstream_path(&path, &upstream_model, stream_request)?;
    let mut upstream_url = route.resolve_upstream_url(&upstream_path, upstream_protocol)?;
    if bridge == ProtocolBridge::Passthrough {
        if let Some(q) = parts.uri.query() {
            upstream_url.set_query(Some(q));
        }
    } else if matches!(
        bridge,
        ProtocolBridge::CompletionsToGemini
            | ProtocolBridge::MessagesToGemini
            | ProtocolBridge::ResponsesToGemini
    ) && stream_request
    {
        upstream_url.query_pairs_mut().append_pair("alt", "sse");
    } else if let Some(q) = parts.uri.query() {
        upstream_url.set_query(Some(q));
    }

    {
        let mut ctx = (*call_ctx).clone();
        ctx.attach_upstream_url(upstream_url.as_str());
        call_ctx = Arc::new(ctx);
    }

    let model_policy = model_access_policy(&cfg);
    let model_request = ModelCallRequest {
        run_id: capture_route.root_session.clone().map(RunId::new),
        attempt_id: None,
        storyline_id: Some(StorylineId::new(capture_route.session_id.clone())),
        call_id: call.call_id.clone(),
        client_model: client_model.clone(),
        upstream_model: upstream_model.clone(),
        provider: provider.as_str().to_string(),
        protocol: protocol.as_str().to_string(),
        upstream_host: upstream_url.host_str().unwrap_or_default().to_string(),
    };
    let mut control = persisting_agentctl::ControlMachine::new();
    let control_transition = control
        .authorize(
            state.control_controller.as_ref(),
            persisting_agentctl::ControlRequest::Model {
                policy: &model_policy,
                request: &model_request,
            },
        )
        .expect("policy controller must return a valid authorization transition");
    if !control_transition.is_allowed() {
        let reason = control_transition.reason;
        let _applied_control = control
            .applied()
            .expect("a denied model control transition can be applied");
        tracing::warn!(
            target: "persisting_gateway",
            run_id = capture_route.root_session.as_deref().unwrap_or("-"),
            storyline_id = %capture_route.session_id,
            call_id = %call.call_id,
            client_model = %client_model,
            upstream_model = %upstream_model,
            provider = provider.as_str(),
            reason = reason.code(),
            "pVisor denied model call"
        );
        return Ok((
            StatusCode::FORBIDDEN,
            format!(
                "persisting-gateway gateway: pVisor denied model `{client_model}` ({})",
                reason.code()
            ),
        )
            .into_response());
    }
    let _applied_control = control
        .applied()
        .expect("an allowed model control transition can be applied");

    if debug_on {
        let body_preview = truncate_body_bytes(&upstream_body);
        debug::log_llm_request(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            protocol.as_str(),
            &path,
            upstream_url.as_str(),
            &body_preview,
        );
    }

    let (_, auth_source) = match resolve_upstream_api_key(route, &parts.headers) {
        Ok(v) => v,
        Err(e) => {
            if debug_on {
                debug::log_llm_upstream_error(
                    state.storage.as_path(),
                    &session_id,
                    &agent_id,
                    &client_model,
                    upstream_url.as_str(),
                    &format!("auth: {e:#}"),
                );
            }
            return Err(e);
        }
    };
    if debug_on {
        debug::log_llm_auth_resolved(state.storage.as_path(), &session_id, auth_source);
    }

    let mut upstream_req = state.client.request(method, upstream_url.clone());
    upstream_req = upstream_req.body(upstream_body.clone());
    upstream_req =
        match apply_upstream_headers(upstream_req, &parts.headers, route, upstream_protocol) {
            Ok(r) => r,
            Err(e) => {
                if debug_on {
                    debug::log_llm_upstream_error(
                        state.storage.as_path(),
                        &session_id,
                        &agent_id,
                        &client_model,
                        upstream_url.as_str(),
                        &format!("auth: {e:#}"),
                    );
                }
                return Err(e);
            }
        };
    if debug_on {
        debug::log_llm_upstream_sending(
            state.storage.as_path(),
            &session_id,
            upstream_url.as_str(),
        );
    }

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            if debug_on {
                debug::log_llm_upstream_error(
                    state.storage.as_path(),
                    &session_id,
                    &agent_id,
                    &client_model,
                    upstream_url.as_str(),
                    &e.to_string(),
                );
            }
            return Err(anyhow::anyhow!("upstream request: {e}"));
        }
    };
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    if debug_on {
        let content_type = resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        debug::log_llm_upstream_headers(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            upstream_url.as_str(),
            status.as_u16(),
            content_type,
            stream_request,
        );
    }

    if !status.is_success() {
        let raw_error = read_response_body_limited(upstream_resp, MAX_RESPONSE_BODY_BYTES).await?;
        let client_error = translate_error_for_bridge(bridge, &raw_error, status)?;
        let body_was_rewritten = client_error != raw_error;
        state.capture_engine.spawn_apply(
            Arc::clone(&call_ctx),
            Event::ResponseComplete(CompleteEvent {
                status: status.as_u16(),
                resp_bytes: client_error.clone(),
                streaming: false,
                stream_metrics: None,
                assistant_content: None,
                semantic: None,
                headers: headers_to_vec(&resp_headers),
            }),
        );
        let mut builder = Response::builder().status(status);
        for (name, value) in &resp_headers {
            if body_was_rewritten && skip_response_header_when_body_changed(name.as_str()) {
                continue;
            }
            builder = builder.header(name, value);
        }
        if body_was_rewritten {
            builder = builder.header("content-type", "application/json");
        }
        builder = attach_capture_headers(builder, &call);
        return Ok(builder
            .body(Body::from(client_error))
            .map_err(|e| anyhow::anyhow!("error response body: {e}"))?
            .into_response());
    }

    if should_stream_to_client(&resp_headers, &body_bytes) {
        // streaming_llm_response takes an owned CallContext so unwrap the Arc when
        // we know we're the only owner (we are — request emit was the only earlier clone).
        let owned_ctx = Arc::try_unwrap(call_ctx).unwrap_or_else(|arc| (*arc).clone());
        return streaming_llm_response(upstream_resp, state, owned_ctx, bridge).await;
    }

    let upstream_bytes = read_response_body_limited(upstream_resp, MAX_RESPONSE_BODY_BYTES).await?;
    let body_was_rewritten = bridge.needs_response_translation();
    let translated =
        translate_response_for_bridge(bridge, &upstream_bytes, protocol, &client_model)?;
    let resp_bytes = translated.body;
    state.capture_engine.spawn_apply(
        Arc::clone(&call_ctx),
        Event::ResponseComplete(CompleteEvent {
            status: status.as_u16(),
            resp_bytes: resp_bytes.clone(),
            streaming: false,
            stream_metrics: None,
            assistant_content: None,
            semantic: Some(translated.semantic),
            headers: headers_to_vec(&resp_headers),
        }),
    );

    let mut builder = Response::builder().status(status);
    // When we rewrote the body the upstream's `content-length` / `content-encoding` /
    // `content-type` no longer apply — drop them and let axum recompute, then re-set
    // a fresh `content-type` matching the new body.
    for (name, value) in resp_headers.iter() {
        if body_was_rewritten && skip_response_header_when_body_changed(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if body_was_rewritten {
        builder = builder.header("content-type", "application/json");
    }
    builder = attach_capture_headers(builder, &call);
    Ok(builder
        .body(Body::from(resp_bytes))
        .map_err(|e| anyhow::anyhow!("response body: {e}"))?
        .into_response())
}

fn gemini_model_from_path(path: &str) -> Option<String> {
    let model = path.split("/models/").nth(1)?.split(':').next()?;
    (!model.is_empty()).then(|| model.to_string())
}

async fn read_response_body_limited(
    response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<bytes::Bytes> {
    if response
        .content_length()
        .is_some_and(|content_length| content_length > limit as u64)
    {
        anyhow::bail!("upstream response body exceeds {limit} bytes");
    }
    let mut body = bytes::BytesMut::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("read upstream response body")?;
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= limit,
            "upstream response body exceeds {limit} bytes"
        );
        body.extend_from_slice(&chunk);
    }
    Ok(body.freeze())
}
