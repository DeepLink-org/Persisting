//! LLM request/response capture handler (non-streaming path + upstream orchestration).

use std::sync::Arc;

use anyhow::Context;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::Value;

use super::auth::{apply_upstream_headers, resolve_upstream_api_key};
use super::common::{
    attach_capture_headers, call_context, effective_config, extract_model, is_models_list_path,
};
use super::http_headers::{is_websocket_upgrade, skip_response_header_when_body_changed};
use super::models_list::build_models_response;
use super::router::resolve_route;
use super::state::ProxyState;
use super::streaming::{should_stream_to_client, streaming_llm_response};
use super::upstream::prepare_upstream_body;
use crate::conversion::{translate_response_for_bridge, ProtocolBridge};
use crate::debug::{self, truncate_body_bytes};
use crate::dialogue_extract::extract_user_message_from_request_body;
use crate::engine::{CompleteEvent, Event, RequestEvent};
use crate::protocol::ProtocolKind;
use crate::session_storage::resolve_capture_route;
use crate::Call;

pub(super) async fn llm_capture(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
    debug_on: bool,
) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();

    if is_websocket_upgrade(&parts.headers) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from(
                "WebSocket transport is not supported by persisting-proxy; use HTTPS",
            ))
            .map_err(|e| anyhow::anyhow!("websocket rejection response: {e}"))?
            .into_response());
    }

    let body_bytes = body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("read request body: {e}"))?
        .to_bytes();
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
    let cfg = effective_config(&state, &capture_route);
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

    state
        .session_clients
        .ensure(state.storage.as_path(), &agent_id, &capture_route, peer);

    let client_model = extract_model(&body_bytes).unwrap_or_else(|| "_unknown".to_string());
    let resolved = resolve_route(&cfg.models, &client_model)?;
    let route = resolved.route;
    let upstream_model = resolved.upstream_model.clone();
    let bridge = ProtocolBridge::needed(protocol, route);
    let upstream_protocol = bridge.upstream_protocol(protocol);
    let provider = route.effective_provider(upstream_protocol);

    // Wrap once in Arc so the request, response (or stream draft), and final events
    // all share a single allocation; clones become refcount bumps.
    let call_ctx: Arc<_> = Arc::new(call_context(
        &state,
        &capture_route,
        &agent_id,
        &call,
        &parts.headers,
        &client_model,
        &upstream_model,
        provider,
        protocol,
        debug_on,
    ));

    {
        let user_content = extract_user_message_from_request_body(&body_bytes);
        let body_json = serde_json::from_slice::<Value>(&body_bytes).ok();
        state.capture_engine.spawn_apply(
            Arc::clone(&call_ctx),
            Event::Request(RequestEvent {
                path: path.clone(),
                body_bytes: body_bytes.len(),
                user_content,
                body_json,
                model_rewritten: resolved.model_rewritten,
            }),
        );
    }

    let upstream_body = prepare_upstream_body(
        &body_bytes,
        resolved.model_rewritten,
        &upstream_model,
        bridge,
        Some(state.reasoning_cache.as_ref()),
    )?;

    let upstream_path = bridge.upstream_path(&path);
    let mut upstream_url = route.resolve_upstream_url(&upstream_path, upstream_protocol)?;
    if let Some(q) = parts.uri.query() {
        upstream_url.set_query(Some(q));
    }

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
    upstream_req = match apply_upstream_headers(upstream_req, &parts.headers, route, protocol) {
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
    let stream_request = super::streaming::request_wants_stream(&upstream_body);

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

    if should_stream_to_client(&resp_headers, &upstream_body) {
        // streaming_llm_response takes an owned CallContext so unwrap the Arc when
        // we know we're the only owner (we are — request emit was the only earlier clone).
        let owned_ctx = Arc::try_unwrap(call_ctx).unwrap_or_else(|arc| (*arc).clone());
        return streaming_llm_response(upstream_resp, state, owned_ctx, bridge).await;
    }

    let mut resp_bytes = upstream_resp.bytes().await?;
    let body_was_rewritten = bridge.needs_response_translation();
    if body_was_rewritten {
        resp_bytes = translate_response_for_bridge(bridge, &resp_bytes, &client_model)?;
    }
    state.capture_engine.spawn_apply(
        Arc::clone(&call_ctx),
        Event::ResponseComplete(CompleteEvent {
            status: status.as_u16(),
            resp_bytes: resp_bytes.clone(),
            streaming: false,
            stream_metrics: None,
            assistant_content: None,
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
