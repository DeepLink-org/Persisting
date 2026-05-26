//! API path → protocol kind (aligned with agentgateway `InputFormat` / `RouteType`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    ChatCompletions,
    Messages,
    Responses,
    Embeddings,
    CountTokens,
    Realtime,
    Detect,
    Unknown,
}

impl ProtocolKind {
    /// Infer from incoming HTTP path (OpenAI-style layout).
    pub fn from_path(path: &str) -> Self {
        let p = path.trim_end_matches('/');
        if p.ends_with("/chat/completions") || p.ends_with("chat/completions") {
            return Self::ChatCompletions;
        }
        if p.ends_with("/messages") || p.contains("/messages") && !p.contains("count_tokens") {
            return Self::Messages;
        }
        if p.ends_with("/responses") {
            return Self::Responses;
        }
        if p.ends_with("/embeddings") {
            return Self::Embeddings;
        }
        if p.contains("count_tokens") || p.ends_with("/count-tokens") {
            return Self::CountTokens;
        }
        if p.contains("/realtime") {
            return Self::Realtime;
        }
        if p.ends_with("/detect") {
            return Self::Detect;
        }
        Self::Unknown
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Messages => "messages",
            Self::Responses => "responses",
            Self::Embeddings => "embeddings",
            Self::CountTokens => "count_tokens",
            Self::Realtime => "realtime",
            Self::Detect => "detect",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "chat_completions" => Self::ChatCompletions,
            "messages" => Self::Messages,
            "responses" => Self::Responses,
            "embeddings" => Self::Embeddings,
            "count_tokens" => Self::CountTokens,
            "realtime" => Self::Realtime,
            "detect" => Self::Detect,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(
            ProtocolKind::from_path("/v1/chat/completions"),
            ProtocolKind::ChatCompletions
        );
        assert_eq!(
            ProtocolKind::from_path("/v1/messages"),
            ProtocolKind::Messages
        );
        assert_eq!(
            ProtocolKind::from_path("/v1/embeddings"),
            ProtocolKind::Embeddings
        );
    }
}
