//! Minimal protocol conversion (Anthropic messages ↔ OpenAI chat completions).

mod messages_completions;
mod stream;

pub use messages_completions::{completions_response_to_messages, messages_request_to_completions};
pub use stream::{translate_completions_sse_to_messages, CompletionsStreamTranslator};

use crate::config::ModelRoute;
use crate::protocol::ProtocolKind;

/// Whether the proxy must translate request/response bodies between protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolBridge {
    /// Same wire format end-to-end.
    Passthrough,
    /// Client `/v1/messages` → upstream `/v1/chat/completions`.
    MessagesToCompletions,
}

impl ProtocolBridge {
    pub fn needed(client: ProtocolKind, route: &ModelRoute) -> Self {
        match client {
            ProtocolKind::Messages if route.upstream_anthropic.is_none() => {
                Self::MessagesToCompletions
            }
            _ => Self::Passthrough,
        }
    }

    pub fn needs_request_translation(self) -> bool {
        matches!(self, Self::MessagesToCompletions)
    }

    pub fn needs_response_translation(self) -> bool {
        matches!(self, Self::MessagesToCompletions)
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
        }
    }

    pub fn upstream_protocol(self, client: ProtocolKind) -> ProtocolKind {
        match self {
            Self::Passthrough => client,
            Self::MessagesToCompletions => ProtocolKind::ChatCompletions,
        }
    }
}
