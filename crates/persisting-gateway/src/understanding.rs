//! Parse provider wire requests into Chronicle's typed LLM event payload.
//!
//! The original JSON remains available for exact wire capture. This module
//! extracts the provider-neutral semantics once, before model rewrite or
//! protocol conversion, so capture and future renderers share one contract.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use persisting_pchronicle::{
    LlmCandidate, LlmContentPart, LlmGenerationParams, LlmImageSource, LlmMessage, LlmProtocol,
    LlmRequest, LlmRequestEventPayload, LlmResponse, LlmResponseEventPayload, LlmResponseFormat,
    LlmRole, LlmToolChoice, LlmToolChoiceMode, LlmToolDefinition, LlmUsage,
};
use serde_json::{Map, Value};

use crate::dialogue_extract::extract_user_message_from_request_body;
use crate::protocol::ProtocolKind;

#[derive(Debug, Clone)]
pub struct ParsedRequest {
    pub body_json: Value,
    pub semantic: Arc<LlmRequestEventPayload>,
    /// Capture-oriented visible text applies product filters (for example
    /// Claude Code system reminders) that do not belong in the semantic IR.
    pub latest_visible_user_content: Option<String>,
}

pub fn understand_request(protocol: ProtocolKind, body: &Bytes) -> Result<ParsedRequest> {
    let body_json: Value = serde_json::from_slice(body).context("parse LLM request")?;
    let semantic = understand_request_value(protocol, &body_json)?;
    Ok(ParsedRequest {
        body_json,
        semantic: Arc::new(semantic),
        latest_visible_user_content: extract_user_message_from_request_body(body),
    })
}

pub fn understand_request_value(
    protocol: ProtocolKind,
    body: &Value,
) -> Result<LlmRequestEventPayload> {
    let object = body
        .as_object()
        .context("LLM request must be a JSON object")?;
    let request = match protocol {
        ProtocolKind::Responses => parse_responses_request(object),
        ProtocolKind::Gemini => parse_gemini_request(object),
        ProtocolKind::Messages => parse_messages_request(object),
        _ => parse_chat_request(object),
    };
    Ok(LlmRequestEventPayload {
        input_format: protocol.into(),
        request,
    })
}

pub fn understand_response_value(
    protocol: ProtocolKind,
    body: &Value,
) -> Result<LlmResponseEventPayload> {
    let object = body
        .as_object()
        .context("LLM response must be a JSON object")?;
    let response = match protocol {
        ProtocolKind::Messages => parse_messages_response(object),
        ProtocolKind::Responses => parse_responses_response(object),
        ProtocolKind::Gemini => parse_gemini_response(object),
        _ => parse_chat_response(object),
    };
    Ok(LlmResponseEventPayload {
        output_format: protocol.into(),
        response,
    })
}

pub fn understand_stream_summary(
    protocol: ProtocolKind,
    model: &str,
    assistant_content: Option<&str>,
    usage: LlmUsage,
) -> LlmResponseEventPayload {
    let candidates = assistant_content
        .filter(|content| !content.is_empty())
        .map(|content| {
            vec![LlmCandidate {
                index: 0,
                message: LlmMessage::new(LlmRole::Assistant, vec![LlmContentPart::text(content)]),
                finish_reason: None,
                extensions: BTreeMap::from([(
                    "capture.completeness".into(),
                    Value::String("visible_text".into()),
                )]),
            }]
        })
        .unwrap_or_default();
    LlmResponseEventPayload {
        output_format: protocol.into(),
        response: LlmResponse {
            id: None,
            model: Some(model.into()),
            candidates,
            usage: Some(usage),
            extensions: BTreeMap::from([("capture.streaming".into(), Value::Bool(true))]),
        },
    }
}

impl From<ProtocolKind> for LlmProtocol {
    fn from(value: ProtocolKind) -> Self {
        match value {
            ProtocolKind::ChatCompletions => Self::ChatCompletions,
            ProtocolKind::Messages => Self::Messages,
            ProtocolKind::Responses => Self::Responses,
            ProtocolKind::Gemini => Self::Gemini,
            _ => Self::Unknown,
        }
    }
}

fn base_request(object: &Map<String, Value>) -> LlmRequest {
    LlmRequest {
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        system: Vec::new(),
        messages: Vec::new(),
        tools: parse_tools(object.get("tools")),
        tool_choice: parse_tool_choice(object.get("tool_choice")),
        generation: parse_generation(object),
        response_format: parse_response_format(object),
        stream: object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        metadata: object.get("metadata").filter(|v| !v.is_null()).cloned(),
        extensions: selected_extensions(object),
    }
}

fn parse_chat_request(object: &Map<String, Value>) -> LlmRequest {
    let mut request = base_request(object);
    for message in object
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_wire_message(message, &mut request);
    }
    request
}

fn parse_chat_response(object: &Map<String, Value>) -> LlmResponse {
    let candidates = object
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(position, choice)| {
            let message = choice.get("message").unwrap_or(choice);
            let mut request = LlmRequest {
                model: None,
                system: Vec::new(),
                messages: Vec::new(),
                tools: Vec::new(),
                tool_choice: None,
                generation: LlmGenerationParams::default(),
                response_format: None,
                stream: false,
                metadata: None,
                extensions: BTreeMap::new(),
            };
            push_wire_message(message, &mut request);
            let message = request
                .messages
                .pop()
                .unwrap_or_else(|| LlmMessage::new(LlmRole::Assistant, Vec::new()));
            LlmCandidate {
                index: choice
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(position),
                message,
                finish_reason: choice
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                extensions: BTreeMap::new(),
            }
        })
        .collect();
    LlmResponse {
        id: object.get("id").and_then(Value::as_str).map(str::to_string),
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        candidates,
        usage: object.get("usage").map(parse_usage),
        extensions: BTreeMap::new(),
    }
}

fn parse_messages_response(object: &Map<String, Value>) -> LlmResponse {
    let parts = parse_content_parts(object.get("content").unwrap_or(&Value::Null));
    let candidates = if parts.is_empty() {
        Vec::new()
    } else {
        vec![LlmCandidate {
            index: 0,
            message: LlmMessage::new(LlmRole::Assistant, parts),
            finish_reason: object
                .get("stop_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            extensions: BTreeMap::new(),
        }]
    };
    LlmResponse {
        id: object.get("id").and_then(Value::as_str).map(str::to_string),
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        candidates,
        usage: object.get("usage").map(parse_usage),
        extensions: BTreeMap::new(),
    }
}

fn parse_responses_response(object: &Map<String, Value>) -> LlmResponse {
    let mut parts = Vec::new();
    for item in object
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => parts.extend(parse_content_parts(
                item.get("content").unwrap_or(&Value::Null),
            )),
            "function_call" | "custom_tool_call" => parts.push(tool_call_part(item)),
            "reasoning" => parts.push(LlmContentPart::Reasoning {
                text: item.get("summary").and_then(value_text),
                signature: item
                    .get("encrypted_content")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            kind if !kind.is_empty() => parts.push(LlmContentPart::Unknown {
                kind: kind.into(),
                value: item.clone(),
            }),
            _ => {}
        }
    }
    let candidates = if parts.is_empty() {
        Vec::new()
    } else {
        vec![LlmCandidate {
            index: 0,
            message: LlmMessage::new(LlmRole::Assistant, parts),
            finish_reason: object
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
            extensions: BTreeMap::new(),
        }]
    };
    LlmResponse {
        id: object.get("id").and_then(Value::as_str).map(str::to_string),
        model: object
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        candidates,
        usage: object.get("usage").map(parse_usage),
        extensions: BTreeMap::new(),
    }
}

fn parse_gemini_response(object: &Map<String, Value>) -> LlmResponse {
    let response_id = object
        .get("responseId")
        .and_then(Value::as_str)
        .unwrap_or("gemini");
    let candidates = object
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(position, candidate)| {
            let index = candidate
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or(position);
            let mut parts = parse_gemini_parts(
                candidate
                    .get("content")
                    .and_then(|content| content.get("parts"))
                    .unwrap_or(&Value::Null),
            );
            for (part_index, part) in parts.iter_mut().enumerate() {
                if let LlmContentPart::ToolCall { id, .. } = part {
                    if id.is_empty() {
                        *id = format!("call_{response_id}_{index}_{part_index}");
                    }
                }
            }
            LlmCandidate {
                index,
                message: LlmMessage::new(LlmRole::Assistant, parts),
                finish_reason: candidate
                    .get("finishReason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                extensions: BTreeMap::new(),
            }
        })
        .collect();
    LlmResponse {
        id: object
            .get("responseId")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: object
            .get("modelVersion")
            .and_then(Value::as_str)
            .map(str::to_string),
        candidates,
        usage: object.get("usageMetadata").map(parse_usage),
        extensions: BTreeMap::new(),
    }
}

fn parse_messages_request(object: &Map<String, Value>) -> LlmRequest {
    let mut request = base_request(object);
    request.system.extend(
        parse_content(object.get("system").unwrap_or(&Value::Null))
            .into_iter()
            .next()
            .map(|parts| LlmMessage::new(LlmRole::System, parts)),
    );
    for message in object
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        push_wire_message(message, &mut request);
    }
    request
}

fn parse_responses_request(object: &Map<String, Value>) -> LlmRequest {
    let mut request = base_request(object);
    if let Some(instructions) = object.get("instructions") {
        let parts = parse_content_parts(instructions);
        if !parts.is_empty() {
            request
                .system
                .push(LlmMessage::new(LlmRole::Developer, parts));
        }
    }
    match object.get("input") {
        Some(Value::String(text)) => request.messages.push(LlmMessage::new(
            LlmRole::User,
            vec![LlmContentPart::text(text)],
        )),
        Some(Value::Array(items)) => {
            let mut pending_calls = Vec::new();
            for item in items {
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
                match item_type {
                    "function_call" | "custom_tool_call" => {
                        pending_calls.push(tool_call_part(item));
                    }
                    "function_call_output" | "custom_tool_call_output" => {
                        flush_pending_calls(&mut pending_calls, &mut request.messages);
                        request.messages.push(LlmMessage::new(
                            LlmRole::Tool,
                            vec![LlmContentPart::ToolResult {
                                call_id: string_field(item, "call_id"),
                                name: item.get("name").and_then(Value::as_str).map(str::to_string),
                                content: item.get("output").cloned().unwrap_or(Value::Null),
                                is_error: item.get("is_error").and_then(Value::as_bool),
                                cache_control: item.get("cache_control").cloned(),
                            }],
                        ));
                    }
                    "reasoning" => {
                        flush_pending_calls(&mut pending_calls, &mut request.messages);
                        request.messages.push(LlmMessage::new(
                            LlmRole::Assistant,
                            vec![LlmContentPart::Reasoning {
                                text: item
                                    .get("summary")
                                    .or_else(|| item.get("content"))
                                    .and_then(value_text),
                                signature: item
                                    .get("encrypted_content")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            }],
                        ));
                    }
                    "message" => {
                        flush_pending_calls(&mut pending_calls, &mut request.messages);
                        push_wire_message(item, &mut request);
                    }
                    _ => {
                        flush_pending_calls(&mut pending_calls, &mut request.messages);
                        let parts = parse_content_parts(item);
                        if !parts.is_empty() {
                            request.messages.push(LlmMessage::new(LlmRole::User, parts));
                        }
                    }
                }
            }
            flush_pending_calls(&mut pending_calls, &mut request.messages);
        }
        _ => {}
    }
    request
}

fn parse_gemini_request(object: &Map<String, Value>) -> LlmRequest {
    let mut request = base_request(object);
    request.stream = false;
    if let Some(instruction) = object.get("systemInstruction") {
        let parts = parse_gemini_parts(instruction.get("parts").unwrap_or(instruction));
        if !parts.is_empty() {
            request.system.push(LlmMessage::new(LlmRole::System, parts));
        }
    }
    for (message_index, content) in object
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let role = match content.get("role").and_then(Value::as_str) {
            Some("model") => LlmRole::Assistant,
            _ => LlmRole::User,
        };
        let mut parts = parse_gemini_parts(content.get("parts").unwrap_or(&Value::Null));
        for (part_index, part) in parts.iter_mut().enumerate() {
            if let LlmContentPart::ToolCall { id, .. } = part {
                if id.is_empty() {
                    *id = format!("call-{message_index}-{part_index}");
                }
            }
        }
        if !parts.is_empty() {
            request.messages.push(LlmMessage::new(role, parts));
        }
    }
    if let Some(config) = object.get("generationConfig").and_then(Value::as_object) {
        request.generation = parse_generation(config);
        request.response_format = parse_gemini_response_format(config);
    }
    request.tools = parse_gemini_tools(object.get("tools"));
    request.tool_choice = parse_gemini_tool_choice(object.get("toolConfig"));
    request
}

fn push_wire_message(message: &Value, request: &mut LlmRequest) {
    let role = parse_role(
        message
            .get("role")
            .or_else(|| message.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("user"),
    );
    let mut parts = if role == LlmRole::Tool {
        vec![LlmContentPart::ToolResult {
            call_id: string_field(message, "tool_call_id"),
            name: message
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            content: message.get("content").cloned().unwrap_or(Value::Null),
            is_error: None,
            cache_control: message.get("cache_control").cloned(),
        }]
    } else {
        parse_content_parts(message.get("content").unwrap_or(&Value::Null))
    };
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        parts.extend(calls.iter().map(tool_call_part));
    }
    if let Some(reasoning) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|reasoning| !reasoning.is_empty())
    {
        parts.insert(
            0,
            LlmContentPart::Reasoning {
                text: Some(reasoning.into()),
                signature: message
                    .get("reasoning_signature")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
        );
    }
    if let Some(call) = message.get("function_call") {
        parts.push(tool_call_part(call));
    }
    if parts.is_empty() {
        return;
    }
    let mut parsed = LlmMessage::new(role.clone(), parts);
    parsed.name = message
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string);
    request.messages.push(parsed);
}

fn parse_content(value: &Value) -> Option<Vec<LlmContentPart>> {
    let parts = parse_content_parts(value);
    (!parts.is_empty()).then_some(parts)
}

fn parse_content_parts(value: &Value) -> Vec<LlmContentPart> {
    match value {
        Value::String(text) => vec![LlmContentPart::text(text)],
        Value::Array(parts) => parts.iter().filter_map(parse_content_part).collect(),
        Value::Object(_) => parse_content_part(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_content_part(part: &Value) -> Option<LlmContentPart> {
    let kind = part.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "text" | "input_text" | "output_text" => Some(LlmContentPart::Text {
            text: string_field(part, "text"),
            cache_control: part
                .get("cache_control")
                .cloned()
                .or_else(|| part.get("prompt_cache_breakpoint").cloned()),
        }),
        "image_url" | "input_image" => {
            let image = part.get("image_url").unwrap_or(part);
            let url = image
                .as_str()
                .or_else(|| image.get("url").and_then(Value::as_str))?;
            Some(LlmContentPart::Image {
                source: image_source(url),
                media_type: image
                    .get("mime_type")
                    .or_else(|| image.get("format"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                detail: image
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
        "image" => parse_anthropic_image(part),
        "tool_use" | "function_call" | "custom_tool_call" => Some(tool_call_part(part)),
        "tool_result" | "function_call_output" | "custom_tool_call_output" => {
            Some(LlmContentPart::ToolResult {
                call_id: part
                    .get("tool_use_id")
                    .or_else(|| part.get("call_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: part.get("name").and_then(Value::as_str).map(str::to_string),
                content: part
                    .get("content")
                    .or_else(|| part.get("output"))
                    .cloned()
                    .unwrap_or(Value::Null),
                is_error: part.get("is_error").and_then(Value::as_bool),
                cache_control: part.get("cache_control").cloned(),
            })
        }
        "thinking" | "reasoning" => Some(LlmContentPart::Reasoning {
            text: part
                .get("thinking")
                .or_else(|| part.get("text"))
                .or_else(|| part.get("summary"))
                .and_then(value_text),
            signature: part
                .get("signature")
                .or_else(|| part.get("encrypted_content"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        _ => value_text(part).map(LlmContentPart::text).or_else(|| {
            (!kind.is_empty()).then(|| LlmContentPart::Unknown {
                kind: kind.to_string(),
                value: part.clone(),
            })
        }),
    }
}

fn parse_anthropic_image(part: &Value) -> Option<LlmContentPart> {
    let source = part.get("source")?;
    let image_source = match source.get("type").and_then(Value::as_str) {
        Some("base64") => LlmImageSource::Data {
            data: string_field(source, "data"),
        },
        Some("url") => LlmImageSource::Url {
            url: string_field(source, "url"),
        },
        _ => return None,
    };
    Some(LlmContentPart::Image {
        source: image_source,
        media_type: source
            .get("media_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        detail: None,
    })
}

fn parse_gemini_parts(value: &Value) -> Vec<LlmContentPart> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    return Some(LlmContentPart::Reasoning {
                        text: Some(text.into()),
                        signature: part
                            .get("thoughtSignature")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
                return Some(LlmContentPart::Text {
                    text: text.into(),
                    cache_control: None,
                });
            }
            if let Some(data) = part.get("inlineData") {
                return Some(LlmContentPart::Image {
                    source: LlmImageSource::Data {
                        data: string_field(data, "data"),
                    },
                    media_type: data
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    detail: None,
                });
            }
            if let Some(file) = part.get("fileData") {
                return Some(LlmContentPart::Image {
                    source: LlmImageSource::File {
                        uri: string_field(file, "fileUri"),
                    },
                    media_type: file
                        .get("mimeType")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    detail: None,
                });
            }
            if let Some(call) = part.get("functionCall") {
                return Some(LlmContentPart::ToolCall {
                    id: call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    name: string_field(call, "name"),
                    arguments: call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new())),
                    signature: part
                        .get("thoughtSignature")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            part.get("functionResponse")
                .map(|response| LlmContentPart::ToolResult {
                    call_id: response
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    name: response
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    content: response.get("response").cloned().unwrap_or(Value::Null),
                    is_error: None,
                    cache_control: None,
                })
        })
        .collect()
}

fn parse_tools(value: Option<&Value>) -> Vec<LlmToolDefinition> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            let function = tool.get("function").unwrap_or(tool);
            let name = function.get("name")?.as_str()?.to_string();
            let kind = tool
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("function")
                .to_string();
            let mut extensions = BTreeMap::new();
            for (key, value) in tool.as_object().into_iter().flatten() {
                if !matches!(
                    key.as_str(),
                    "type" | "name" | "description" | "input_schema" | "function"
                ) {
                    extensions.insert(format!("wire.{key}"), value.clone());
                }
            }
            Some(LlmToolDefinition {
                kind,
                name,
                description: function
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: function
                    .get("parameters")
                    .or_else(|| function.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}})),
                strict: function.get("strict").and_then(Value::as_bool),
                extensions,
            })
        })
        .collect()
}

fn parse_gemini_tools(value: Option<&Value>) -> Vec<LlmToolDefinition> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("functionDeclarations")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|function| {
            Some(LlmToolDefinition {
                kind: "function".into(),
                name: function.get("name")?.as_str()?.into(),
                description: function
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                input_schema: function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"type":"object","properties":{}})),
                strict: None,
                extensions: BTreeMap::new(),
            })
        })
        .collect()
}

fn parse_tool_choice(value: Option<&Value>) -> Option<LlmToolChoice> {
    let value = value?;
    let (mode, name) = match value {
        Value::String(mode) => (
            match mode.as_str() {
                "none" => LlmToolChoiceMode::None,
                "required" | "any" => LlmToolChoiceMode::Required,
                _ => LlmToolChoiceMode::Auto,
            },
            None,
        ),
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str).unwrap_or("auto");
            let name = object
                .get("name")
                .or_else(|| object.get("function").and_then(|f| f.get("name")))
                .and_then(Value::as_str)
                .map(str::to_string);
            let mode = if kind == "tool" || kind == "function" {
                LlmToolChoiceMode::Tool
            } else if kind == "any" || kind == "required" {
                LlmToolChoiceMode::Required
            } else if kind == "none" {
                LlmToolChoiceMode::None
            } else {
                LlmToolChoiceMode::Auto
            };
            (mode, name)
        }
        _ => return None,
    };
    Some(LlmToolChoice {
        mode,
        name,
        parallel: value
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            .map(|disabled| !disabled),
        extensions: BTreeMap::new(),
    })
}

fn parse_gemini_tool_choice(value: Option<&Value>) -> Option<LlmToolChoice> {
    let config = value?.get("functionCallingConfig")?;
    let mode = match config.get("mode").and_then(Value::as_str) {
        Some("NONE") => LlmToolChoiceMode::None,
        Some("ANY") => {
            if config
                .get("allowedFunctionNames")
                .and_then(Value::as_array)
                .is_some_and(|names| names.len() == 1)
            {
                LlmToolChoiceMode::Tool
            } else {
                LlmToolChoiceMode::Required
            }
        }
        _ => LlmToolChoiceMode::Auto,
    };
    let name = config
        .get("allowedFunctionNames")
        .and_then(Value::as_array)
        .and_then(|names| names.first())
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(LlmToolChoice {
        mode,
        name,
        parallel: None,
        extensions: BTreeMap::new(),
    })
}

fn parse_generation(object: &Map<String, Value>) -> LlmGenerationParams {
    let stop_sequences = object
        .get("stop")
        .or_else(|| object.get("stop_sequences"))
        .map(|stop| match stop {
            Value::String(value) => vec![value.clone()],
            Value::Array(values) => values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();
    LlmGenerationParams {
        temperature: number_f64(object, &["temperature"]),
        top_p: number_f64(object, &["top_p", "topP"]),
        top_k: number_u64(object, &["top_k", "topK"]),
        max_output_tokens: number_u64(
            object,
            &[
                "max_tokens",
                "max_output_tokens",
                "max_completion_tokens",
                "maxOutputTokens",
            ],
        ),
        stop_sequences,
        seed: object.get("seed").and_then(Value::as_i64),
        frequency_penalty: number_f64(object, &["frequency_penalty", "frequencyPenalty"]),
        presence_penalty: number_f64(object, &["presence_penalty", "presencePenalty"]),
        candidate_count: number_u64(object, &["n", "candidateCount"]),
        reasoning_effort: object
            .get("reasoning_effort")
            .or_else(|| object.get("reasoning"))
            .or_else(|| object.get("thinking"))
            .filter(|value| !value.is_null())
            .cloned(),
        thinking_budget: object
            .get("thinkingConfig")
            .and_then(|value| value.get("thinkingBudget"))
            .and_then(Value::as_i64),
        extensions: BTreeMap::new(),
    }
}

fn parse_response_format(object: &Map<String, Value>) -> Option<LlmResponseFormat> {
    let format = object
        .get("response_format")
        .or_else(|| object.get("text").and_then(|text| text.get("format")))
        .or_else(|| {
            object
                .get("output_config")
                .and_then(|value| value.get("format"))
        })?;
    let kind = format
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("json_schema")
        .to_string();
    let json_schema = format.get("json_schema").unwrap_or(format);
    Some(LlmResponseFormat {
        kind,
        name: json_schema
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        schema: json_schema.get("schema").cloned(),
        strict: json_schema.get("strict").and_then(Value::as_bool),
        extensions: BTreeMap::new(),
    })
}

fn parse_gemini_response_format(object: &Map<String, Value>) -> Option<LlmResponseFormat> {
    let mime = object.get("responseMimeType")?.as_str()?;
    Some(LlmResponseFormat {
        kind: mime.into(),
        name: None,
        schema: object.get("responseJsonSchema").cloned(),
        strict: None,
        extensions: BTreeMap::new(),
    })
}

fn selected_extensions(object: &Map<String, Value>) -> BTreeMap<String, Value> {
    let mut extensions = BTreeMap::new();
    for (source, name) in [
        ("cache_control", "anthropic.cache_control"),
        ("thinking", "anthropic.thinking"),
        ("output_config", "anthropic.output_config"),
        ("cachedContent", "google.cached_content"),
        ("safetySettings", "google.safety_settings"),
        ("labels", "google.labels"),
        ("parallel_tool_calls", "openai.parallel_tool_calls"),
        ("user", "openai.user"),
    ] {
        if let Some(value) = object.get(source).filter(|value| !value.is_null()) {
            extensions.insert(name.into(), value.clone());
        }
    }
    extensions
}

fn parse_usage(value: &Value) -> LlmUsage {
    let input_tokens = first_u64(
        value,
        &["input_tokens", "prompt_tokens", "promptTokenCount"],
    );
    let output_tokens = first_u64(
        value,
        &["output_tokens", "completion_tokens", "candidatesTokenCount"],
    );
    let cache_read_tokens = first_u64(
        value,
        &["cache_read_input_tokens", "cachedContentTokenCount"],
    )
    .max(
        value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    let cache_write_tokens = first_u64(value, &["cache_creation_input_tokens"]);
    let reasoning_tokens = first_u64(value, &["thoughtsTokenCount"]).max(
        value
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    LlmUsage {
        input_tokens,
        output_tokens,
        total_tokens: first_u64(value, &["total_tokens", "totalTokenCount"])
            .max(input_tokens.saturating_add(output_tokens)),
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
    }
}

fn first_u64(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .unwrap_or(0)
}

fn tool_call_part(value: &Value) -> LlmContentPart {
    let function = value.get("function").unwrap_or(value);
    let arguments = function
        .get("arguments")
        .or_else(|| value.get("arguments"))
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let arguments = match arguments {
        Value::String(raw) if raw.trim().is_empty() => Value::Object(Map::new()),
        Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        value => value,
    };
    LlmContentPart::ToolCall {
        id: value
            .get("id")
            .or_else(|| value.get("call_id"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        name: function
            .get("name")
            .or_else(|| value.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        arguments,
        signature: value
            .get("signature")
            .or_else(|| value.get("thoughtSignature"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn flush_pending_calls(calls: &mut Vec<LlmContentPart>, messages: &mut Vec<LlmMessage>) {
    if !calls.is_empty() {
        messages.push(LlmMessage::new(LlmRole::Assistant, std::mem::take(calls)));
    }
}

fn parse_role(role: &str) -> LlmRole {
    match role {
        "system" => LlmRole::System,
        "developer" => LlmRole::Developer,
        "assistant" | "model" => LlmRole::Assistant,
        "tool" | "function" => LlmRole::Tool,
        _ => LlmRole::User,
    }
}

fn image_source(url: &str) -> LlmImageSource {
    if url.starts_with("data:") {
        LlmImageSource::Data { data: url.into() }
    } else if url.starts_with("gs://") {
        LlmImageSource::File { uri: url.into() }
    } else {
        LlmImageSource::Url { url: url.into() }
    }
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(value_text)
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("content"))
            .and_then(value_text),
        _ => None,
    }
}

fn string_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn number_f64(object: &Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_f64))
}

fn number_u64(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_messages_semantics_without_chat_completions_hop() {
        let body = json!({
            "model":"claude-test",
            "system":[{"type":"text","text":"system","cache_control":{"type":"ephemeral"}}],
            "messages":[
                {"role":"user","content":[{"type":"text","text":"hello"}]},
                {"role":"assistant","content":[{"type":"tool_use","id":"call-1","name":"shell","input":{"cmd":"pwd"}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"ok"}]}
            ],
            "tools":[{"name":"shell","input_schema":{"type":"object"}}],
            "max_tokens":64,
            "stream":true
        });
        let parsed = understand_request_value(ProtocolKind::Messages, &body).unwrap();
        assert_eq!(parsed.input_format, LlmProtocol::Messages);
        assert_eq!(parsed.request.model.as_deref(), Some("claude-test"));
        assert_eq!(parsed.request.system.len(), 1);
        assert_eq!(parsed.request.messages.len(), 3);
        assert_eq!(parsed.request.tool_names(), ["shell"]);
        assert_eq!(parsed.request.visible_user_turns(), 1);
        assert_eq!(parsed.request.generation.max_output_tokens, Some(64));
    }

    #[test]
    fn parses_gemini_native_contents_for_capture() {
        let body = json!({
            "systemInstruction":{"parts":[{"text":"system"}]},
            "contents":[
                {"role":"user","parts":[{"text":"hello"}]},
                {"role":"model","parts":[{"functionCall":{"name":"shell","args":{"cmd":"pwd"}},"thoughtSignature":"sig"}]}
            ],
            "generationConfig":{"maxOutputTokens":32,"temperature":0.2}
        });
        let parsed = understand_request_value(ProtocolKind::Gemini, &body).unwrap();
        assert_eq!(parsed.request.latest_user_text().as_deref(), Some("hello"));
        assert_eq!(parsed.request.generation.max_output_tokens, Some(32));
        assert!(matches!(
            parsed.request.messages[1].parts[0],
            LlmContentPart::ToolCall { ref signature, .. } if signature.as_deref() == Some("sig")
        ));
    }

    #[test]
    fn parses_responses_tool_sequence() {
        let body = json!({
            "model":"gpt-test",
            "instructions":"system",
            "input":[
                {"type":"message","role":"user","content":[{"type":"input_text","text":"run"}]},
                {"type":"function_call","call_id":"call-1","name":"shell","arguments":"{\"cmd\":\"pwd\"}"},
                {"type":"function_call_output","call_id":"call-1","output":"ok"}
            ]
        });
        let parsed = understand_request_value(ProtocolKind::Responses, &body).unwrap();
        assert_eq!(parsed.request.system.len(), 1);
        assert_eq!(parsed.request.messages.len(), 3);
        assert!(matches!(
            parsed.request.messages[1].parts[0],
            LlmContentPart::ToolCall { ref id, .. } if id == "call-1"
        ));
    }

    #[test]
    fn parses_gemini_response_usage_and_tool_call() {
        let body = json!({
            "responseId":"response-1",
            "modelVersion":"gemini-test",
            "candidates":[{
                "index":0,
                "content":{"role":"model","parts":[{
                    "functionCall":{"name":"shell","args":{"cmd":"pwd"}},
                    "thoughtSignature":"sig"
                }]},
                "finishReason":"STOP"
            }],
            "usageMetadata":{
                "promptTokenCount":3,
                "candidatesTokenCount":2,
                "thoughtsTokenCount":1,
                "totalTokenCount":6
            }
        });
        let parsed = understand_response_value(ProtocolKind::Gemini, &body).unwrap();
        assert_eq!(parsed.response.id.as_deref(), Some("response-1"));
        assert_eq!(parsed.response.usage.unwrap().reasoning_tokens, 1);
        assert!(matches!(
            parsed.response.candidates[0].message.parts[0],
            LlmContentPart::ToolCall { ref signature, .. } if signature.as_deref() == Some("sig")
        ));
    }
}
