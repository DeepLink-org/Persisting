//! Minimal protocol conversion (Anthropic messages / OpenAI responses ↔ chat completions).

mod messages_completions;
mod responses_completions;
mod responses_stream;
mod stream;
mod tool_call;

pub use messages_completions::{completions_response_to_messages, messages_request_to_completions};
pub use responses_completions::{
    completions_response_to_responses, responses_request_to_completions,
};
pub use responses_stream::{
    translate_completions_sse_to_responses, CompletionsToResponsesStreamTranslator,
};
pub use stream::{translate_completions_sse_to_messages, CompletionsStreamTranslator};
pub use tool_call::{decode_stream_arguments_delta, unquote_chat_tool_arguments};

use crate::config::ModelRoute;
use crate::protocol::ProtocolKind;
use bytes::Bytes;

/// Whether the proxy must translate request/response bodies between protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolBridge {
    /// Same wire format end-to-end.
    Passthrough,
    /// Client `/v1/messages` → upstream `/v1/chat/completions`.
    MessagesToCompletions,
    /// Client `/v1/responses` → upstream `/v1/chat/completions` (Codex + DeepSeek).
    ResponsesToCompletions,
}

impl ProtocolBridge {
    pub fn needed(client: ProtocolKind, route: &ModelRoute) -> Self {
        match client {
            ProtocolKind::Messages if route.upstream_anthropic.is_none() => {
                Self::MessagesToCompletions
            }
            ProtocolKind::Responses if !route_supports_native_responses(route) => {
                Self::ResponsesToCompletions
            }
            _ => Self::Passthrough,
        }
    }

    pub fn needs_request_translation(self) -> bool {
        !matches!(self, Self::Passthrough)
    }

    pub fn needs_response_translation(self) -> bool {
        !matches!(self, Self::Passthrough)
    }

    pub fn upstream_path(self, client_path: &str) -> String {
        match self {
            Self::Passthrough => client_path.to_string(),
            Self::MessagesToCompletions => {
                if client_path.contains("/v1/") {
                    client_path.replacen("/messages", "/chat/completions", 1)
                } else {
                    "/v1/chat/completions".to_string()
                }
            }
            Self::ResponsesToCompletions => {
                if client_path.contains("/v1/") {
                    client_path.replacen("/responses", "/chat/completions", 1)
                } else {
                    "/v1/chat/completions".to_string()
                }
            }
        }
    }

    pub fn upstream_protocol(self, client: ProtocolKind) -> ProtocolKind {
        match self {
            Self::Passthrough => client,
            Self::MessagesToCompletions | Self::ResponsesToCompletions => {
                ProtocolKind::ChatCompletions
            }
        }
    }
}

/// Translate client request body to upstream wire format for [`ProtocolBridge`].
pub fn translate_request_for_bridge(
    bridge: ProtocolBridge,
    body: &Bytes,
    upstream_model: &str,
    reasoning_cache: Option<&crate::proxy::reasoning::ReasoningCacheHandle>,
) -> anyhow::Result<Bytes> {
    match bridge {
        ProtocolBridge::Passthrough => Ok(body.clone()),
        ProtocolBridge::MessagesToCompletions => {
            messages_request_to_completions(body, upstream_model)
        }
        ProtocolBridge::ResponsesToCompletions => {
            responses_request_to_completions(body, upstream_model, reasoning_cache)
        }
    }
}

/// Translate upstream response body back to the client wire format for [`ProtocolBridge`].
pub fn translate_response_for_bridge(
    bridge: ProtocolBridge,
    body: &Bytes,
    client_model: &str,
) -> anyhow::Result<Bytes> {
    match bridge {
        ProtocolBridge::Passthrough => Ok(body.clone()),
        ProtocolBridge::MessagesToCompletions => {
            completions_response_to_messages(body, client_model)
        }
        ProtocolBridge::ResponsesToCompletions => {
            completions_response_to_responses(body, client_model)
        }
    }
}

/// Streaming response translator selected by [`ProtocolBridge`].
pub enum StreamTranslator {
    ToMessages(CompletionsStreamTranslator),
    ToResponses(CompletionsToResponsesStreamTranslator),
}

impl StreamTranslator {
    pub fn new(bridge: ProtocolBridge, client_model: &str) -> Option<Self> {
        match bridge {
            ProtocolBridge::MessagesToCompletions => Some(Self::ToMessages(
                CompletionsStreamTranslator::new(client_model),
            )),
            ProtocolBridge::ResponsesToCompletions => Some(Self::ToResponses(
                CompletionsToResponsesStreamTranslator::new(client_model),
            )),
            ProtocolBridge::Passthrough => None,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> anyhow::Result<String> {
        match self {
            Self::ToMessages(t) => t.push_chunk(chunk),
            Self::ToResponses(t) => t.push_chunk(chunk),
        }
    }

    pub fn finish_stream(&mut self) -> anyhow::Result<String> {
        match self {
            Self::ToMessages(t) => t.finish_stream(),
            Self::ToResponses(t) => t.finish_stream(),
        }
    }

    pub fn metrics(&self) -> &crate::usage::StreamMetrics {
        match self {
            Self::ToMessages(t) => t.metrics(),
            Self::ToResponses(t) => t.metrics(),
        }
    }

    pub fn upstream_snapshot(&self) -> &str {
        match self {
            Self::ToMessages(t) => t.upstream_snapshot(),
            Self::ToResponses(t) => t.upstream_snapshot(),
        }
    }

    pub fn accumulated_assistant_text(&self) -> Option<String> {
        self.streaming_capture_snapshot()
    }

    pub fn streaming_capture_snapshot(&self) -> Option<String> {
        let text = match self {
            Self::ToMessages(t) => t.accumulated_assistant_text().to_string(),
            // Markdown + turn indexing: narrative only; tools stay in Vortex payload.
            Self::ToResponses(t) => t.narrative_for_capture(),
        };
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    pub fn drain_reasoning_snapshot(&mut self) -> (Vec<String>, String) {
        match self {
            Self::ToMessages(_) => (Vec::new(), String::new()),
            Self::ToResponses(t) => t.drain_reasoning_snapshot(),
        }
    }
}

fn route_supports_native_responses(route: &ModelRoute) -> bool {
    route.upstream.as_deref().is_some_and(|u| {
        let lower = u.to_ascii_lowercase();
        lower.contains("api.openai.com") || lower.contains("openai.azure.com")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyConfig;

    fn deepseek_route() -> ModelRoute {
        ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[[models]]
name = "deepseek-chat"
upstream = "https://api.deepseek.com/v1"
"#,
        )
        .unwrap()
        .models
        .into_iter()
        .next()
        .unwrap()
    }

    #[test]
    fn responses_to_completions_for_deepseek() {
        let route = deepseek_route();
        assert_eq!(
            ProtocolBridge::needed(ProtocolKind::Responses, &route),
            ProtocolBridge::ResponsesToCompletions
        );
        assert_eq!(
            ProtocolBridge::ResponsesToCompletions.upstream_path("/v1/responses"),
            "/v1/chat/completions"
        );
    }

    #[test]
    fn responses_passthrough_for_openai_upstream() {
        let route = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[[models]]
name = "gpt-5"
upstream = "https://api.openai.com/v1"
"#,
        )
        .unwrap()
        .models
        .into_iter()
        .next()
        .unwrap();
        assert_eq!(
            ProtocolBridge::needed(ProtocolKind::Responses, &route),
            ProtocolBridge::Passthrough
        );
    }
}
