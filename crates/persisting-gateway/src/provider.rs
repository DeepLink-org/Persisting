//! LLM gateway provider kinds.

use serde::{Deserialize, Serialize};

#[cfg(test)]
use proptest::prelude::*;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn provider_strategy() -> impl Strategy<Value = ProviderKind> {
        prop_oneof![
            Just(ProviderKind::OpenAi),
            Just(ProviderKind::Anthropic),
            Just(ProviderKind::Gemini),
            Just(ProviderKind::Vertex),
            Just(ProviderKind::Bedrock),
            Just(ProviderKind::Azure),
            Just(ProviderKind::Copilot),
            Just(ProviderKind::Custom),
        ]
    }

    fn mixed_case(value: &str, uppercase: bool) -> String {
        if uppercase {
            value.to_ascii_uppercase()
        } else {
            value.to_ascii_lowercase()
        }
    }

    proptest! {
        #[test]
        fn canonical_names_roundtrip_through_parse(kind in provider_strategy()) {
            prop_assert_eq!(ProviderKind::parse(kind.as_str()), kind);
        }

        #[test]
        fn canonical_names_are_case_insensitive(
            kind in provider_strategy(),
            uppercase in any::<bool>(),
        ) {
            prop_assert_eq!(ProviderKind::parse(&mixed_case(kind.as_str(), uppercase)), kind);
        }

        #[test]
        fn openai_accepts_both_public_spellings(uppercase in any::<bool>()) {
            prop_assert_eq!(
                ProviderKind::parse(&mixed_case("openai", uppercase)),
                ProviderKind::OpenAi,
            );
            prop_assert_eq!(
                ProviderKind::parse(&mixed_case("open_ai", uppercase)),
                ProviderKind::OpenAi,
            );
        }

        #[test]
        fn default_hosts_match_only_providers_with_builtin_endpoints(kind in provider_strategy()) {
            let expected = match kind {
                ProviderKind::OpenAi => Some("api.openai.com"),
                ProviderKind::Anthropic => Some("api.anthropic.com"),
                ProviderKind::Gemini => Some("generativelanguage.googleapis.com"),
                ProviderKind::Copilot => Some("api.githubcopilot.com"),
                ProviderKind::Vertex
                | ProviderKind::Bedrock
                | ProviderKind::Azure
                | ProviderKind::Custom => None,
            };
            prop_assert_eq!(kind.default_host(), expected);
        }
    }
}
