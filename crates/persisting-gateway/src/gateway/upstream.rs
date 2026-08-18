//! Prepare upstream LLM request body after routing (model rewrite + protocol bridge).

use anyhow::Result;
use bytes::Bytes;

use crate::conversion::{translate_request_for_bridge, ProtocolBridge};

use super::model::rewrite_model_in_body;
use super::reasoning::ReasoningCacheHandle;

/// Build the request body sent to the upstream LLM after routing.
pub fn prepare_upstream_body(
    client_body: &Bytes,
    semantic: Option<&persisting_pchronicle::model::LlmRequestEventPayload>,
    model_rewritten: bool,
    upstream_model: &str,
    bridge: ProtocolBridge,
    reasoning_cache: Option<&ReasoningCacheHandle>,
) -> Result<Bytes> {
    if bridge.needs_request_translation() && !client_body.is_empty() {
        let semantic = semantic.ok_or_else(|| {
            anyhow::anyhow!("protocol translation requires a Chronicle typed LLM request")
        })?;
        return translate_request_for_bridge(bridge, semantic, upstream_model, reasoning_cache);
    }
    if model_rewritten {
        return rewrite_model_in_body(client_body, upstream_model);
    }
    Ok(client_body.clone())
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
            None,
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
            None,
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
