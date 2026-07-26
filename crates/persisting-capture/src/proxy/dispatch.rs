//! HTTP entry: route CONNECT / forward / LLM capture.

use std::sync::atomic::Ordering;

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use bytes::Bytes;
use http_body_util::BodyExt;
use persisting_proto::{NetworkAccessRequest, NetworkTransport, RunId, StorylineId};

use super::common::effective_config;
use super::forward::{
    handle_connect_authorized, is_forward_proxy_request, is_llm_capture_path,
    transparent_forward_authorized,
};
use super::llm_capture::llm_capture;
use super::network_policy::{
    authorize_egress, forbidden_response, host_from_authority, NetworkPolicy,
};
use super::state::ProxyState;
use crate::debug::{self, is_debug_enabled};
use crate::session_storage::resolve_capture_route;

pub fn build_router(state: ProxyState) -> Router {
    Router::new().fallback(any(proxy_handler)).with_state(state)
}

async fn proxy_handler(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    State(state): State<ProxyState>,
    req: Request,
) -> Response {
    match dispatch(state, req, peer).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("proxy error: {e:#}");
            (StatusCode::BAD_GATEWAY, format!("persisting-proxy: {e:#}")).into_response()
        }
    }
}

async fn dispatch(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
) -> anyhow::Result<Response> {
    state.active_requests.fetch_add(1, Ordering::Relaxed);
    let result = dispatch_impl(state.clone(), req, peer).await;
    state.active_requests.fetch_sub(1, Ordering::Relaxed);
    result
}

fn deny_egress(
    state: &ProxyState,
    policy: &NetworkPolicy,
    host: &str,
    reason: &super::network_policy::DenyReason,
    session_id: &str,
    debug_on: bool,
) -> Response {
    if debug_on {
        debug::log_network_denied(
            state.storage.as_path(),
            host,
            policy.mode_str(),
            reason.as_str(),
            session_id,
        );
    }
    let (status, msg) = forbidden_response(host, reason);
    (status, msg).into_response()
}

async fn dispatch_impl(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
) -> anyhow::Result<Response> {
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let log_route = resolve_capture_route(
        req.headers(),
        &Bytes::new(),
        &state.config.session_header,
        state.storage.as_path(),
    );
    let session_id = log_route.session_id.clone();
    let cfg = effective_config(&state, &log_route);
    let debug_on = is_debug_enabled(&cfg, state.storage.as_path());
    let policy = NetworkPolicy::from_config(&cfg)?;

    if *req.method() == Method::CONNECT {
        let authority = req
            .uri()
            .authority()
            .map(|a| a.to_string())
            .unwrap_or_else(|| uri.clone());
        let host = host_from_authority(&authority);
        if let Err(reason) = authorize_egress(
            state.access_controller.as_ref(),
            &policy,
            &NetworkAccessRequest {
                run_id: log_route.root_session.clone().map(RunId),
                attempt_id: None,
                storyline_id: Some(StorylineId(log_route.session_id.clone())),
                host: host.clone(),
                port: req.uri().port_u16(),
                transport: NetworkTransport::TcpTunnel,
            },
        ) {
            return Ok(deny_egress(
                &state,
                &policy,
                &host,
                &reason,
                &session_id,
                debug_on,
            ));
        }
        if debug_on {
            debug::log_connect(state.storage.as_path(), &authority, &session_id);
        }
        return Ok(handle_connect_authorized(req).await);
    }

    let path = req.uri().path().to_string();
    if is_forward_proxy_request(req.method(), req.uri()) {
        let host = req.uri().host().map(str::to_string).unwrap_or_default();
        let transport = if req.uri().scheme_str() == Some("https") {
            NetworkTransport::Https
        } else {
            NetworkTransport::Http
        };
        if let Err(reason) = authorize_egress(
            state.access_controller.as_ref(),
            &policy,
            &NetworkAccessRequest {
                run_id: log_route.root_session.clone().map(RunId),
                attempt_id: None,
                storyline_id: Some(StorylineId(log_route.session_id.clone())),
                host: host.clone(),
                port: req.uri().port_u16(),
                transport,
            },
        ) {
            return Ok(deny_egress(
                &state,
                &policy,
                &host,
                &reason,
                &session_id,
                debug_on,
            ));
        }
        if is_llm_capture_path(&path) {
            if debug_on {
                debug::log_dispatch(
                    state.storage.as_path(),
                    method.as_str(),
                    &uri,
                    &session_id,
                    "llm_capture",
                );
            }
            return llm_capture(state, req, peer, debug_on).await;
        }
        if debug_on {
            debug::log_dispatch(
                state.storage.as_path(),
                method.as_str(),
                &uri,
                &session_id,
                "forward",
            );
        }
        let resp = transparent_forward_authorized(&state.client, req).await?;
        if debug_on {
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp
                .into_body()
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("read forward body: {e}"))?
                .to_bytes();
            debug::log_forward(
                state.storage.as_path(),
                method.as_str(),
                &uri,
                &session_id,
                status.as_u16(),
                &String::from_utf8_lossy(&bytes),
            );
            let mut builder = Response::builder().status(status);
            for (name, value) in headers.iter() {
                builder = builder.header(name, value);
            }
            return Ok(builder
                .body(Body::from(bytes))
                .map_err(|e| anyhow::anyhow!("forward response: {e}"))?
                .into_response());
        }
        return Ok(resp.into_response());
    }

    if debug_on {
        debug::log_dispatch(
            state.storage.as_path(),
            method.as_str(),
            &uri,
            &session_id,
            "llm_gateway",
        );
    }
    // Relative-path LLM gateway on `listen` — not subject to egress allowlist.
    llm_capture(state, req, peer, debug_on).await
}
