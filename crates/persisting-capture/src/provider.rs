//! LLM vendor kinds (subset of agentgateway `AIProvider`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    #[default]
    OpenAi,
    Anthropic,
    Gemini,
    Vertex,
    Bedrock,
    Azure,
    Copilot,
    /// Custom upstream only (passthrough).
    Custom,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "openai" | "open_ai" => Self::OpenAi,
            "anthropic" => Self::Anthropic,
            "gemini" => Self::Gemini,
            "vertex" => Self::Vertex,
            "bedrock" => Self::Bedrock,
            "azure" => Self::Azure,
            "copilot" => Self::Copilot,
            "custom" => Self::Custom,
            _ => Self::Custom,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::Vertex => "vertex",
            Self::Bedrock => "bedrock",
            Self::Azure => "azure",
            Self::Copilot => "copilot",
            Self::Custom => "custom",
        }
    }

    /// Default API host when `upstream` is not set (agentgateway defaults).
    pub fn default_host(self) -> Option<&'static str> {
        match self {
            Self::OpenAi => Some("api.openai.com"),
            Self::Anthropic => Some("api.anthropic.com"),
            Self::Gemini => Some("generativelanguage.googleapis.com"),
            Self::Copilot => Some("api.githubcopilot.com"),
            Self::Vertex | Self::Bedrock | Self::Azure | Self::Custom => None,
        }
    }
}
