//! Provider-neutral LLM semantics embedded in Chronicle trajectory events.
//!
//! These types are the stable semantic boundary between protocol adapters and
//! Chronicle storage. They intentionally do not model HTTP framing: exact
//! client/upstream wire bodies remain in the event's `http` payload.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type LlmExtensions = BTreeMap<String, Value>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProtocol {
    ChatCompletions,
    Messages,
    Responses,
    Gemini,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmRole {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmContentPart {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    Image {
        source: LlmImageSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolResult {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        content: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<Value>,
    },
    /// A lossless escape hatch for provider content that has no canonical form.
    Unknown { kind: String, value: Value },
}

impl LlmContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            cache_control: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmImageSource {
    Url { url: String },
    Data { data: String },
    File { uri: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    #[serde(default)]
    pub parts: Vec<LlmContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

impl LlmMessage {
    pub fn new(role: LlmRole, parts: Vec<LlmContentPart>) -> Self {
        Self {
            role,
            parts,
            name: None,
            extensions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmToolDefinition {
    #[serde(default = "default_function_kind")]
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

fn default_function_kind() -> String {
    "function".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmToolChoiceMode {
    Auto,
    None,
    Required,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmToolChoice {
    pub mode: LlmToolChoiceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmGenerationParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponseFormat {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    /// System/developer instructions kept separately from conversational turns
    /// while preserving their original role and ordering.
    pub system: Vec<LlmMessage>,
    #[serde(default)]
    pub messages: Vec<LlmMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<LlmToolDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<LlmToolChoice>,
    #[serde(default)]
    pub generation: LlmGenerationParams,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<LlmResponseFormat>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

impl LlmRequest {
    pub fn visible_user_turns(&self) -> usize {
        self.messages
            .iter()
            .filter(|message| {
                message.role == LlmRole::User
                    && message.parts.iter().any(|part| {
                        matches!(part, LlmContentPart::Text { text, .. } if !text.trim().is_empty())
                            || matches!(part, LlmContentPart::Image { .. })
                    })
            })
            .count()
    }

    pub fn latest_user_text(&self) -> Option<String> {
        self.messages.iter().rev().find_map(|message| {
            (message.role == LlmRole::User)
                .then(|| {
                    message
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            LlmContentPart::Text { text, .. } if !text.trim().is_empty() => {
                                Some(text.as_str())
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|text| !text.is_empty())
        })
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name.clone()).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequestEventPayload {
    pub input_format: LlmProtocol,
    pub request: LlmRequest,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCandidate {
    #[serde(default)]
    pub index: usize,
    pub message: LlmMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub candidates: Vec<LlmCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsage>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: LlmExtensions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponseEventPayload {
    pub output_format: LlmProtocol,
    pub response: LlmResponse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LlmStreamEvent {
    Start {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    TextDelta {
        candidate: usize,
        text: String,
    },
    ReasoningDelta {
        candidate: usize,
        text: String,
    },
    ToolCallStart {
        candidate: usize,
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    ToolArgumentsDelta {
        candidate: usize,
        id: String,
        delta: String,
    },
    Usage {
        usage: LlmUsage,
    },
    Finish {
        candidate: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9 _:/-]{0,24}").unwrap()
    }

    fn role_strategy() -> impl Strategy<Value = LlmRole> {
        prop::sample::select(vec![
            LlmRole::System,
            LlmRole::Developer,
            LlmRole::User,
            LlmRole::Assistant,
            LlmRole::Tool,
        ])
    }

    fn content_part_strategy() -> impl Strategy<Value = LlmContentPart> {
        prop_oneof![
            token_strategy().prop_map(LlmContentPart::text),
            token_strategy().prop_map(|url| LlmContentPart::Image {
                source: LlmImageSource::Url { url },
                media_type: None,
                detail: None,
            }),
            token_strategy().prop_map(|text| LlmContentPart::Reasoning {
                text: Some(text),
                signature: None,
            }),
            token_strategy().prop_map(|id| LlmContentPart::ToolCall {
                id,
                name: "tool".into(),
                arguments: serde_json::json!({"ok": true}),
                signature: None,
            }),
        ]
    }

    fn message_strategy() -> impl Strategy<Value = LlmMessage> {
        (
            role_strategy(),
            proptest::collection::vec(content_part_strategy(), 0..4),
            token_strategy(),
        )
            .prop_map(|(role, parts, name)| LlmMessage {
                role,
                parts,
                name: (!name.is_empty()).then_some(name),
                extensions: BTreeMap::new(),
            })
    }

    fn request_strategy() -> impl Strategy<Value = LlmRequest> {
        (
            prop::option::of(token_strategy()),
            proptest::collection::vec(message_strategy(), 0..12),
            proptest::collection::vec(token_strategy(), 0..4),
            any::<bool>(),
        )
            .prop_map(|(model, messages, tool_names, stream)| LlmRequest {
                model,
                system: Vec::new(),
                messages,
                tools: tool_names
                    .into_iter()
                    .map(|name| LlmToolDefinition {
                        kind: "function".into(),
                        name,
                        description: None,
                        input_schema: serde_json::json!({"type": "object"}),
                        strict: None,
                        extensions: BTreeMap::new(),
                    })
                    .collect(),
                tool_choice: None,
                generation: LlmGenerationParams::default(),
                response_format: None,
                stream,
                metadata: None,
                extensions: BTreeMap::new(),
            })
    }

    #[test]
    fn request_payload_roundtrips_and_derives_summary() {
        let payload = LlmRequestEventPayload {
            input_format: LlmProtocol::Messages,
            request: LlmRequest {
                model: Some("claude-test".into()),
                system: vec![LlmMessage::new(
                    LlmRole::System,
                    vec![LlmContentPart::text("be concise")],
                )],
                messages: vec![LlmMessage::new(
                    LlmRole::User,
                    vec![LlmContentPart::text("hello")],
                )],
                tools: vec![LlmToolDefinition {
                    kind: "function".into(),
                    name: "shell".into(),
                    description: None,
                    input_schema: serde_json::json!({"type":"object"}),
                    strict: None,
                    extensions: BTreeMap::new(),
                }],
                tool_choice: None,
                generation: LlmGenerationParams::default(),
                response_format: None,
                stream: true,
                metadata: None,
                extensions: BTreeMap::new(),
            },
        };

        let value = serde_json::to_value(&payload).unwrap();
        let decoded: LlmRequestEventPayload = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(decoded.request.visible_user_turns(), 1);
        assert_eq!(decoded.request.latest_user_text().as_deref(), Some("hello"));
        assert_eq!(decoded.request.tool_names(), ["shell"]);
    }

    #[test]
    fn stream_events_are_explicitly_typed() {
        let event = LlmStreamEvent::ToolArgumentsDelta {
            candidate: 0,
            id: "call-1".into(),
            delta: "{\"path\":".into(),
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["type"], "tool_arguments_delta");
        assert_eq!(value["id"], "call-1");
    }

    proptest! {
        #[test]
        fn generated_request_payloads_roundtrip_losslessly(
            protocol in prop::sample::select(vec![
                LlmProtocol::ChatCompletions,
                LlmProtocol::Messages,
                LlmProtocol::Responses,
                LlmProtocol::Gemini,
                LlmProtocol::Unknown,
            ]),
            request in request_strategy(),
        ) {
            let payload = LlmRequestEventPayload { input_format: protocol, request };
            let encoded = serde_json::to_value(&payload).unwrap();
            prop_assert_eq!(serde_json::from_value::<LlmRequestEventPayload>(encoded).unwrap(), payload);
        }

        #[test]
        fn request_summaries_match_the_message_content(
            request in request_strategy(),
        ) {
            let expected_visible = request.messages.iter().filter(|message| {
                message.role == LlmRole::User && message.parts.iter().any(|part| {
                    matches!(part, LlmContentPart::Text { text, .. } if !text.trim().is_empty())
                        || matches!(part, LlmContentPart::Image { .. })
                })
            }).count();
            prop_assert_eq!(request.visible_user_turns(), expected_visible);

            let expected_latest = request.messages.iter().rev().find_map(|message| {
                (message.role == LlmRole::User).then(|| {
                    message.parts.iter().filter_map(|part| match part {
                        LlmContentPart::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n")
                }).filter(|text| !text.is_empty())
            });
            prop_assert_eq!(request.latest_user_text(), expected_latest);
            prop_assert_eq!(request.tool_names(), request.tools.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>());
        }

        #[test]
        fn stream_events_roundtrip_with_their_explicit_variant(
            candidate in 0usize..4,
            text in token_strategy(),
            id in token_strategy(),
            reason in prop::option::of(token_strategy()),
            usage in (0u64..1000, 0u64..1000, 0u64..2000).prop_map(|(input, output, total)| LlmUsage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: total,
                ..LlmUsage::default()
            }),
        ) {
            let variants = vec![
                LlmStreamEvent::TextDelta { candidate, text: text.clone() },
                LlmStreamEvent::ReasoningDelta { candidate, text: text.clone() },
                LlmStreamEvent::ToolCallStart { candidate, id: id.clone(), name: text.clone(), signature: reason.clone() },
                LlmStreamEvent::ToolArgumentsDelta { candidate, id: id.clone(), delta: text.clone() },
                LlmStreamEvent::Usage { usage: usage.clone() },
                LlmStreamEvent::Finish { candidate, reason: reason.clone() },
                LlmStreamEvent::Error { message: text.clone(), code: reason.clone() },
            ];
            for event in variants {
                let encoded = serde_json::to_value(&event).unwrap();
                prop_assert_eq!(serde_json::from_value::<LlmStreamEvent>(encoded).unwrap(), event);
            }
        }
    }
}
