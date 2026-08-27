//! LLM protocol conversion around Chat Completions and Gemini native generateContent.

mod gemini_native;
mod messages_completions;
mod responses_completions;
mod semantic;
mod tool_call;
mod typed_stream;

/// Maximum accepted JSON request body. Accepted bodies are retained verbatim
/// for replay, so the capture and conversion paths share the same bound.
pub const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Maximum buffered non-streaming provider response.
pub const MAX_RESPONSE_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Maximum single incomplete SSE frame retained by a translator.
pub const MAX_SSE_FRAME_BYTES: usize = 2 * 1024 * 1024;
/// Maximum complete streaming response retained for durable raw capture.
pub const MAX_STREAM_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

pub use gemini_native::{completions_request_to_gemini, gemini_response_to_completions};
pub use messages_completions::{completions_response_to_messages, messages_request_to_completions};
pub use responses_completions::{
    completions_response_to_responses, responses_request_to_completions,
};
pub use tool_call::{decode_stream_arguments_delta, unquote_chat_tool_arguments};
pub use typed_stream::TypedStreamTranslator as StreamTranslator;

use crate::config::ModelRoute;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
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
    /// Client `/v1/chat/completions` → Gemini native `generateContent`.
    CompletionsToGemini,
    /// Client `/v1/messages` → Chat Completions → Gemini native `generateContent`.
    MessagesToGemini,
    /// Client `/v1/responses` → Chat Completions → Gemini native `generateContent`.
    ResponsesToGemini,
}

impl ProtocolBridge {
    pub fn needed(client: ProtocolKind, route: &ModelRoute) -> Self {
        if route.provider_kind() == ProviderKind::Gemini {
            return match client {
                ProtocolKind::ChatCompletions => Self::CompletionsToGemini,
                ProtocolKind::Messages => Self::MessagesToGemini,
                ProtocolKind::Responses => Self::ResponsesToGemini,
                _ => Self::Passthrough,
            };
        }
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

    pub fn upstream_path(
        self,
        client_path: &str,
        model: &str,
        streaming: bool,
    ) -> anyhow::Result<String> {
        match self {
            Self::Passthrough => Ok(client_path.to_string()),
            Self::MessagesToCompletions => {
                if client_path.contains("/v1/") {
                    Ok(client_path.replacen("/messages", "/chat/completions", 1))
                } else {
                    Ok("/v1/chat/completions".to_string())
                }
            }
            Self::ResponsesToCompletions => {
                if client_path.contains("/v1/") {
                    Ok(client_path.replacen("/responses", "/chat/completions", 1))
                } else {
                    Ok("/v1/chat/completions".to_string())
                }
            }
            Self::CompletionsToGemini | Self::MessagesToGemini | Self::ResponsesToGemini => {
                let model = model.strip_prefix("models/").unwrap_or(model);
                anyhow::ensure!(
                    !model.is_empty()
                        && model
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte)),
                    "invalid Gemini model id `{model}`"
                );
                let method = if streaming {
                    "streamGenerateContent"
                } else {
                    "generateContent"
                };
                Ok(format!("/v1beta/models/{model}:{method}"))
            }
        }
    }

    pub fn upstream_protocol(self, client: ProtocolKind) -> ProtocolKind {
        match self {
            Self::Passthrough => client,
            Self::MessagesToCompletions | Self::ResponsesToCompletions => {
                ProtocolKind::ChatCompletions
            }
            Self::CompletionsToGemini | Self::MessagesToGemini | Self::ResponsesToGemini => {
                ProtocolKind::Gemini
            }
        }
    }
}

/// Translate client request body to upstream wire format for [`ProtocolBridge`].
pub fn translate_request_for_bridge(
    bridge: ProtocolBridge,
    semantic: &persisting_pchronicle::model::LlmRequestEventPayload,
    upstream_model: &str,
    reasoning_cache: Option<&crate::gateway::ReasoningCacheHandle>,
) -> anyhow::Result<Bytes> {
    match bridge {
        ProtocolBridge::Passthrough => anyhow::bail!("passthrough requests have no renderer"),
        ProtocolBridge::MessagesToCompletions | ProtocolBridge::ResponsesToCompletions => {
            semantic::request_to_chat_completions(semantic, upstream_model, reasoning_cache)
        }
        ProtocolBridge::CompletionsToGemini
        | ProtocolBridge::MessagesToGemini
        | ProtocolBridge::ResponsesToGemini => {
            semantic::request_to_gemini(semantic, upstream_model)
        }
    }
}

/// A provider response after its single parse into Chronicle semantics.
pub struct TranslatedResponse {
    pub body: Bytes,
    pub semantic: std::sync::Arc<persisting_pchronicle::model::LlmResponseEventPayload>,
}

/// Parse an upstream response once and render it to the client wire protocol.
pub fn translate_response_for_bridge(
    bridge: ProtocolBridge,
    body: &Bytes,
    client_protocol: ProtocolKind,
    client_model: &str,
) -> anyhow::Result<TranslatedResponse> {
    let upstream_protocol = bridge.upstream_protocol(client_protocol);
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let semantic = crate::understanding::understand_response_value(upstream_protocol, &value)?;
    let rendered = if bridge == ProtocolBridge::Passthrough {
        body.clone()
    } else {
        semantic::response_to_wire(&semantic, client_protocol.into(), client_model)?
    };
    Ok(TranslatedResponse {
        body: rendered,
        semantic: std::sync::Arc::new(semantic),
    })
}

/// Translate an upstream error without running it through a success-body parser.
pub fn translate_error_for_bridge(
    bridge: ProtocolBridge,
    body: &Bytes,
    status: axum::http::StatusCode,
) -> anyhow::Result<Bytes> {
    match bridge {
        ProtocolBridge::MessagesToCompletions => openai_error_to_messages(body, status),
        ProtocolBridge::MessagesToGemini => {
            let openai = gemini_native::translate_gemini_error(body)?;
            openai_error_to_messages(&openai, status)
        }
        ProtocolBridge::CompletionsToGemini | ProtocolBridge::ResponsesToGemini => {
            gemini_native::translate_gemini_error(body)
        }
        // Responses and Chat Completions use the same OpenAI-compatible error envelope.
        ProtocolBridge::ResponsesToCompletions | ProtocolBridge::Passthrough => Ok(body.clone()),
    }
}

fn openai_error_to_messages(body: &Bytes, status: axum::http::StatusCode) -> anyhow::Result<Bytes> {
    let value: serde_json::Value = serde_json::from_slice(body).unwrap_or_else(
        |_| serde_json::json!({"error": {"message": String::from_utf8_lossy(body)}}),
    );
    let error = value.get("error").unwrap_or(&value);
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("upstream request failed");
    let error_type = error
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| normalized_anthropic_error_type(status));
    Ok(Bytes::from(serde_json::to_vec(&serde_json::json!({
        "type": "error",
        "error": {"type": error_type, "message": message},
    }))?))
}

fn normalized_anthropic_error_type(status: axum::http::StatusCode) -> &'static str {
    match status {
        axum::http::StatusCode::BAD_REQUEST => "invalid_request_error",
        axum::http::StatusCode::UNAUTHORIZED | axum::http::StatusCode::FORBIDDEN => {
            "authentication_error"
        }
        axum::http::StatusCode::NOT_FOUND => "not_found_error",
        axum::http::StatusCode::TOO_MANY_REQUESTS => "rate_limit_error",
        _ => "api_error",
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
    use proptest::prelude::*;

    fn text_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9][a-zA-Z0-9 .,!?_-]{0,63}"
    }

    #[test]
    fn response_bridge_parses_once_then_renders_messages() {
        let upstream = Bytes::from_static(
            br#"{"id":"chat-1","model":"upstream","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1,"total_tokens":3}}"#,
        );
        let translated = translate_response_for_bridge(
            ProtocolBridge::MessagesToCompletions,
            &upstream,
            ProtocolKind::Messages,
            "client-model",
        )
        .unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&translated.body).unwrap();
        assert_eq!(wire["model"], "client-model");
        assert_eq!(wire["content"][0]["text"], "hello");
        assert_eq!(
            translated.semantic.output_format,
            persisting_pchronicle::model::LlmProtocol::ChatCompletions
        );
        assert_eq!(
            translated
                .semantic
                .response
                .usage
                .as_ref()
                .unwrap()
                .total_tokens,
            3
        );
    }

    #[test]
    fn gemini_response_renders_responses_without_chat_intermediate() {
        let upstream = Bytes::from_static(
            br#"{"responseId":"g-1","modelVersion":"gemini-upstream","candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"hello"}]},"finishReason":"STOP"}]}"#,
        );
        let translated = translate_response_for_bridge(
            ProtocolBridge::ResponsesToGemini,
            &upstream,
            ProtocolKind::Responses,
            "client-model",
        )
        .unwrap();
        let wire: serde_json::Value = serde_json::from_slice(&translated.body).unwrap();
        assert_eq!(wire["model"], "client-model");
        assert_eq!(wire["output"][0]["content"][0]["text"], "hello");
        assert_eq!(
            translated.semantic.output_format,
            persisting_pchronicle::model::LlmProtocol::Gemini
        );
    }

    proptest! {
        #[test]
        fn responses_bridge_selects_chat_completions_for_deepseek(
            model in "deepseek-[a-z][a-z0-9-]{0,20}",
            streaming in any::<bool>(),
        ) {
            let config = format!(
                "listen = \"127.0.0.1:1\"\n\n[[models]]\nname = \"{model}\"\nupstream = \"https://api.deepseek.com/v1\"\n"
            );
            let route = ProxyConfig::from_toml_str(&config)
                .unwrap()
                .models
                .into_iter()
                .next()
                .unwrap();
            let bridge = ProtocolBridge::needed(ProtocolKind::Responses, &route);

            prop_assert_eq!(bridge, ProtocolBridge::ResponsesToCompletions);
            prop_assert_eq!(
                bridge.upstream_protocol(ProtocolKind::Responses),
                ProtocolKind::ChatCompletions
            );
            prop_assert_eq!(
                bridge
                    .upstream_path("/v1/responses", &model, streaming)
                    .unwrap(),
                "/v1/chat/completions"
            );
        }

        #[test]
        fn responses_bridge_passthrough_covers_native_openai_hosts(
            model in "[a-z][a-z0-9._-]{0,24}",
            host in prop_oneof![Just("api.openai.com"), Just("openai.azure.com")],
        ) {
            let config = format!(
                "listen = \"127.0.0.1:1\"\n\n[[models]]\nname = \"{model}\"\nupstream = \"https://{host}/v1\"\n"
            );
            let route = ProxyConfig::from_toml_str(&config)
                .unwrap()
                .models
                .into_iter()
                .next()
                .unwrap();

            prop_assert_eq!(
                ProtocolBridge::needed(ProtocolKind::Responses, &route),
                ProtocolBridge::Passthrough
            );
        }

        #[test]
        fn messages_error_conversion_preserves_generated_error_details(
            message in text_strategy(),
            error_type in "[a-z][a-z0-9_-]{0,16}",
            code in prop::option::of("[a-z][a-z0-9_-]{0,16}"),
            status_code in prop_oneof![Just(400u16), Just(429u16), Just(500u16)],
        ) {
            let body = Bytes::from(serde_json::to_vec(&serde_json::json!({
                "error": {
                    "message": message.clone(),
                    "type": error_type.clone(),
                    "code": code,
                }
            })).unwrap());
            let translated = translate_error_for_bridge(
                ProtocolBridge::MessagesToCompletions,
                &body,
                axum::http::StatusCode::from_u16(status_code).unwrap(),
            ).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&translated).unwrap();

            prop_assert_eq!(value["type"].as_str(), Some("error"));
            prop_assert_eq!(value["error"]["type"].as_str(), Some(error_type.as_str()));
            prop_assert_eq!(value["error"]["message"].as_str(), Some(message.as_str()));
        }

        #[test]
        fn responses_error_conversion_preserves_arbitrary_bytes(
            body in prop::collection::vec(any::<u8>(), 0..=256),
            status_code in prop_oneof![Just(400u16), Just(429u16), Just(500u16)],
        ) {
            let body = Bytes::from(body);
            let translated = translate_error_for_bridge(
                ProtocolBridge::ResponsesToCompletions,
                &body,
                axum::http::StatusCode::from_u16(status_code).unwrap(),
            ).unwrap();

            prop_assert_eq!(translated, body);
        }

        #[test]
        fn gemini_bridge_preserves_model_and_stream_path(
            model in "[a-z][a-z0-9._-]{0,24}",
            prefixed in any::<bool>(),
            streaming in any::<bool>(),
        ) {
            let route_model = if prefixed {
                format!("models/{model}")
            } else {
                model.clone()
            };
            let config = format!(
                "listen = \"127.0.0.1:1\"\n\n[[models]]\nname = \"{route_model}\"\nprovider = \"gemini\"\nupstream = \"https://generativelanguage.googleapis.com/v1beta\"\n"
            );
            let route = ProxyConfig::from_toml_str(&config)
                .unwrap()
                .models
                .into_iter()
                .next()
                .unwrap();
            let bridge = ProtocolBridge::needed(ProtocolKind::ChatCompletions, &route);
            let method = if streaming {
                "streamGenerateContent"
            } else {
                "generateContent"
            };

            prop_assert_eq!(bridge, ProtocolBridge::CompletionsToGemini);
            prop_assert_eq!(
                bridge.upstream_protocol(ProtocolKind::ChatCompletions),
                ProtocolKind::Gemini
            );
            prop_assert_eq!(
                bridge
                    .upstream_path("/v1/chat/completions", &route_model, streaming)
                    .unwrap(),
                format!("/v1beta/models/{model}:{method}")
            );
        }

        #[test]
        fn messages_request_conversion_preserves_text_and_stream_flag(
            text in text_strategy(),
            system in prop::option::of(text_strategy()),
            stream in any::<bool>(),
        ) {
            let request = serde_json::json!({
                "model": "claude-test",
                "system": system.clone(),
                "messages": [{"role": "user", "content": text.clone()}],
                "stream": stream,
            });
            let translated = messages_request_to_completions(
                &Bytes::from(serde_json::to_vec(&request).unwrap()),
                "upstream-model",
            )
            .unwrap();
            let wire: serde_json::Value = serde_json::from_slice(&translated).unwrap();

            prop_assert_eq!(wire["model"].as_str(), Some("upstream-model"));
            prop_assert_eq!(wire["stream"].as_bool(), Some(stream));
            let user_index = if let Some(system) = system.as_ref() {
                prop_assert_eq!(wire["messages"][0]["role"].as_str(), Some("system"));
                prop_assert_eq!(wire["messages"][0]["content"].as_str(), Some(system.as_str()));
                1
            } else {
                0
            };
            prop_assert_eq!(wire["messages"][user_index]["role"].as_str(), Some("user"));
            prop_assert_eq!(wire["messages"][user_index]["content"][0]["type"].as_str(), Some("text"));
            prop_assert_eq!(
                wire["messages"][user_index]["content"][0]["text"].as_str(),
                Some(text.as_str())
            );
            prop_assert!(wire.get("system").is_none());
        }

        #[test]
        fn responses_request_conversion_preserves_text_and_stream_flag(
            text in text_strategy(),
            instructions in prop::option::of(text_strategy()),
            stream in any::<bool>(),
        ) {
            let request = serde_json::json!({
                "model": "responses-test",
                "instructions": instructions.clone(),
                "input": text.clone(),
                "stream": stream,
            });
            let translated = responses_request_to_completions(
                &Bytes::from(serde_json::to_vec(&request).unwrap()),
                "upstream-model",
                None,
            )
            .unwrap();
            let wire: serde_json::Value = serde_json::from_slice(&translated).unwrap();

            prop_assert_eq!(wire["model"].as_str(), Some("upstream-model"));
            prop_assert_eq!(wire["stream"].as_bool(), Some(stream));
            let user_index = if let Some(instructions) = instructions.as_ref() {
                prop_assert_eq!(wire["messages"][0]["role"].as_str(), Some("system"));
                prop_assert_eq!(wire["messages"][0]["content"].as_str(), Some(instructions.as_str()));
                1
            } else {
                0
            };
            prop_assert_eq!(wire["messages"][user_index]["role"].as_str(), Some("user"));
            prop_assert_eq!(wire["messages"][user_index]["content"].as_str(), Some(text.as_str()));
        }

        #[test]
        fn completion_response_conversion_preserves_wire_shape_and_usage(
            text in text_strategy(),
            model in "[a-z][a-z0-9_-]{0,24}",
            finish_reason in prop_oneof![Just("stop"), Just("length"), Just("content_filter")],
            input_tokens in 0u64..=10_000,
            output_tokens in 0u64..=10_000,
        ) {
            let response = serde_json::json!({
                "id": "chatcmpl-property",
                "model": "upstream-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": text},
                    "finish_reason": finish_reason
                }],
                "usage": {
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": input_tokens + output_tokens
                }
            });
            let body = Bytes::from(serde_json::to_vec(&response).unwrap());

            let messages = completions_response_to_messages(&body, &model).unwrap();
            let messages_wire: serde_json::Value = serde_json::from_slice(&messages).unwrap();
            let expected_stop = match finish_reason {
                "length" => "max_tokens",
                "content_filter" => "refusal",
                _ => "end_turn",
            };
            prop_assert_eq!(messages_wire["type"].as_str(), Some("message"));
            prop_assert_eq!(messages_wire["role"].as_str(), Some("assistant"));
            prop_assert_eq!(messages_wire["model"].as_str(), Some(model.as_str()));
            prop_assert_eq!(messages_wire["content"][0]["type"].as_str(), Some("text"));
            prop_assert_eq!(messages_wire["content"][0]["text"].as_str(), Some(text.as_str()));
            prop_assert_eq!(messages_wire["stop_reason"].as_str(), Some(expected_stop));
            prop_assert_eq!(messages_wire["usage"]["input_tokens"].as_u64(), Some(input_tokens));
            prop_assert_eq!(messages_wire["usage"]["output_tokens"].as_u64(), Some(output_tokens));

            let responses = completions_response_to_responses(&body, &model).unwrap();
            let responses_wire: serde_json::Value = serde_json::from_slice(&responses).unwrap();
            let expected_status = match finish_reason {
                "length" => "incomplete",
                "content_filter" => "failed",
                _ => "completed",
            };
            prop_assert_eq!(responses_wire["object"].as_str(), Some("response"));
            prop_assert_eq!(responses_wire["status"].as_str(), Some(expected_status));
            prop_assert_eq!(responses_wire["model"].as_str(), Some(model.as_str()));
            prop_assert_eq!(responses_wire["output"][0]["type"].as_str(), Some("message"));
            prop_assert_eq!(responses_wire["output"][0]["content"][0]["type"].as_str(), Some("output_text"));
            prop_assert_eq!(responses_wire["output"][0]["content"][0]["text"].as_str(), Some(text.as_str()));
            prop_assert_eq!(responses_wire["usage"]["input_tokens"].as_u64(), Some(input_tokens));
            prop_assert_eq!(responses_wire["usage"]["output_tokens"].as_u64(), Some(output_tokens));
            prop_assert_eq!(responses_wire["usage"]["total_tokens"].as_u64(), Some(input_tokens + output_tokens));
        }
    }
}
