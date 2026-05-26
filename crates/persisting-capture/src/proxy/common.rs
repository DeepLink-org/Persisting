//! Shared helpers for dispatch and LLM capture handlers.

use std::sync::Arc;

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::Value;

use super::state::ProxyState;
use crate::capture_call::CaptureCall;
use crate::config::ProxyConfig;
use crate::engine::headers_to_vec;
use crate::engine::CaptureInvocation;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::run_config::load_session_proxy_config;
use crate::session_storage::{route_config_key, CaptureRoute};

pub(crate) fn effective_config(state: &ProxyState, route: &CaptureRoute) -> Arc<ProxyConfig> {
    load_session_proxy_config(state.storage.as_path(), route_config_key(route))
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(&state.config))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_invocation(
    state: &ProxyState,
    route: &CaptureRoute,
    agent_id: &str,
    call: &CaptureCall,
    headers: &HeaderMap,
    client_model: &str,
    upstream_model: &str,
    provider: ProviderKind,
    protocol: ProtocolKind,
    debug_on: bool,
) -> CaptureInvocation {
    let cfg = effective_config(state, route);
    CaptureInvocation {
        route: route.clone(),
        agent_id: agent_id.to_string(),
        call: call.clone(),
        request_headers: headers_to_vec(headers),
        level: cfg.capture_level,
        client_model: client_model.to_string(),
        upstream_model: upstream_model.to_string(),
        provider,
        protocol,
        debug_on,
    }
}

pub(crate) fn extract_model(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

pub(crate) fn attach_capture_headers(
    builder: axum::http::response::Builder,
    call: &CaptureCall,
) -> axum::http::response::Builder {
    builder
        .header("x-persisting-call-id", call.call_id.as_str())
        .header("x-persisting-trace-id", call.trace_id.as_str())
}

pub(crate) fn is_models_list_path(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p.ends_with("/models") || p == "models" || p.ends_with("/v1/models")
}
