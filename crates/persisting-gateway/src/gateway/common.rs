//! Shared helpers for LLM gateway capture handlers.

use std::sync::Arc;

use axum::http::HeaderMap;
use bytes::Bytes;
use persisting_agentctl::ModelAccessPolicy;
use serde_json::Value;

use super::state::GatewayState;
use crate::config::{CaptureLevel, ProxyConfig};
use crate::engine::headers_to_vec;
use crate::engine::CallContext;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::runtime::run_config::load_session_proxy_config;
use crate::session::storage::{route_config_key, CaptureRoute};
use crate::Call;

pub(crate) fn effective_config(state: &GatewayState, route: &CaptureRoute) -> Arc<ProxyConfig> {
    load_session_proxy_config(state.storage.as_path(), route_config_key(route))
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(&state.config))
}

pub(crate) fn model_access_policy(config: &ProxyConfig) -> ModelAccessPolicy {
    let allowed_models = config
        .models
        .iter()
        .map(|route| route.name.clone())
        .collect();
    let providers: Vec<String> = config
        .models
        .iter()
        .filter_map(|route| route.provider.clone())
        .collect();
    // An inferred/custom provider must remain representable during migration.
    // An empty provider list means model identity is enforced but provider is open.
    let allowed_providers = if providers.len() == config.models.len() {
        providers
    } else {
        Vec::new()
    };
    ModelAccessPolicy {
        allowed_models,
        allowed_providers,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn call_context(
    route: &CaptureRoute,
    agent_id: &str,
    call: &Call,
    headers: &HeaderMap,
    client_model: &str,
    upstream_model: &str,
    provider: ProviderKind,
    protocol: ProtocolKind,
    capture_level: CaptureLevel,
    debug_on: bool,
) -> CallContext {
    CallContext::new(
        route.clone(),
        agent_id,
        call.clone(),
        headers_to_vec(headers),
        capture_level,
        client_model,
        upstream_model,
        provider,
        protocol,
        debug_on,
    )
}

pub(crate) fn extract_model(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

pub(crate) fn attach_capture_headers(
    builder: axum::http::response::Builder,
    call: &Call,
) -> axum::http::response::Builder {
    builder
        .header("x-persisting-call-id", call.call_id.as_str())
        .header("x-persisting-trace-id", call.trace_id.as_str())
}

pub(crate) fn is_models_list_path(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p.ends_with("/models") || p == "models" || p.ends_with("/v1/models")
}
