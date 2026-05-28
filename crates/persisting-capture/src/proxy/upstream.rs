//! Prepare upstream LLM request body after routing (model rewrite + protocol bridge).

use anyhow::Result;
use bytes::Bytes;

use crate::conversion::{translate_request_for_bridge, ProtocolBridge};

use super::model::rewrite_model_in_body;
use super::reasoning::ReasoningCacheHandle;

/// Build the request body sent to the upstream LLM after routing.
pub fn prepare_upstream_body(
    client_body: &Bytes,
    model_rewritten: bool,
    upstream_model: &str,
    bridge: ProtocolBridge,
    reasoning_cache: Option<&ReasoningCacheHandle>,
) -> Result<Bytes> {
    let mut upstream_body = client_body.clone();
    if model_rewritten {
        upstream_body = rewrite_model_in_body(client_body, upstream_model)?;
    }
    if bridge.needs_request_translation() && !client_body.is_empty() {
        upstream_body =
            translate_request_for_bridge(bridge, &upstream_body, upstream_model, reasoning_cache)?;
    }
    Ok(upstream_body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelRoute;

    fn route_with_upstream(upstream: &str) -> ModelRoute {
        ModelRoute {
            name: "*".into(),
            provider: None,
            upstream: Some(upstream.into()),
            upstream_anthropic: None,
            path_prefix: None,
            api_key_env: None,
            api_key: None,
            forward: None,
        }
    }

    #[test]
    fn passthrough_without_rewrite() {
        let body = Bytes::from_static(br#"{"model":"m","messages":[]}"#);
        let out = prepare_upstream_body(
            &body,
            false,
            "m",
            ProtocolBridge::needed(
                crate::protocol::ProtocolKind::ChatCompletions,
                &route_with_upstream("http://x/v1"),
            ),
            None,
        )
        .unwrap();
        assert_eq!(out, body);
    }

    #[test]
    fn rewrites_model_when_forwarded() {
        let body = Bytes::from_static(br#"{"model":"claude-3","messages":[]}"#);
        let out = prepare_upstream_body(
            &body,
            true,
            "deepseek-chat",
            ProtocolBridge::Passthrough,
            None,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "deepseek-chat");
    }
}
