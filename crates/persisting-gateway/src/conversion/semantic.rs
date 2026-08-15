//! Chronicle typed LLM payload → provider/client wire renderers.
//!
//! This module is the only semantic conversion boundary used by the runtime.
//! Wire JSON is parsed into Chronicle types before entering here; renderers
//! never reinterpret a second protocol-shaped JSON document.

use std::collections::HashMap;

use anyhow::{Context, Result};
use bytes::Bytes;
use persisting_pchronicle::{
    LlmCandidate, LlmContentPart, LlmImageSource, LlmMessage, LlmProtocol, LlmRequest,
    LlmRequestEventPayload, LlmResponse, LlmResponseEventPayload, LlmRole, LlmToolChoiceMode,
    LlmUsage,
};
use serde_json::{json, Value};

pub fn request_to_chat_completions(
    semantic: &LlmRequestEventPayload,
    upstream_model: &str,
    reasoning_cache: Option<&crate::gateway::ReasoningCacheHandle>,
) -> Result<Bytes> {
    let mut rendered =
        render_chat_completions(&semantic.request, &semantic.input_format, upstream_model);
    if semantic.input_format == LlmProtocol::Responses {
        if let Some(cache) = reasoning_cache {
            if let Some(messages) = rendered.get_mut("messages").and_then(Value::as_array_mut) {
                cache.apply_to_messages(messages);
            }
        }
    }
    Ok(Bytes::from(serde_json::to_vec(&rendered)?))
}

pub fn request_to_gemini(semantic: &LlmRequestEventPayload, upstream_model: &str) -> Result<Bytes> {
    let request = &semantic.request;
    let mut calls = HashMap::<String, (String, usize)>::new();
    let mut contents = Vec::new();
    let mut inline_system = Vec::new();

    for message in &request.messages {
        if matches!(message.role, LlmRole::System | LlmRole::Developer) {
            inline_system.extend(message.parts.iter().filter_map(|part| match part {
                LlmContentPart::Text { text, .. } if !text.is_empty() => Some(text.as_str()),
                _ => None,
            }));
            continue;
        }
        let role = if message.role == LlmRole::Assistant {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        let function_response = message
            .parts
            .iter()
            .any(|part| matches!(part, LlmContentPart::ToolResult { .. }));
        for (part_index, part) in message.parts.iter().enumerate() {
            match part {
                LlmContentPart::Text { text, .. } if !text.is_empty() => {
                    parts.push(json!({"text": text}));
                }
                LlmContentPart::Image {
                    source, media_type, ..
                } => parts.push(render_gemini_image(source, media_type.as_deref())?),
                LlmContentPart::Reasoning { text, signature } => {
                    if let Some(text) = text.as_deref().filter(|text| !text.is_empty()) {
                        let mut value = json!({"text": text, "thought": true});
                        if let Some(signature) = signature {
                            value["thoughtSignature"] = json!(signature);
                        }
                        parts.push(value);
                    }
                }
                LlmContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                    signature,
                } => {
                    calls.insert(id.clone(), (name.clone(), part_index));
                    let mut value = json!({
                        "functionCall": {"name": name, "args": arguments}
                    });
                    if let Some(signature) = signature {
                        value["thoughtSignature"] = json!(signature);
                    }
                    parts.push(value);
                }
                LlmContentPart::ToolResult {
                    call_id,
                    name,
                    content,
                    is_error,
                    ..
                } => {
                    let resolved_name = name
                        .as_deref()
                        .or_else(|| calls.get(call_id).map(|(name, _)| name.as_str()))
                        .unwrap_or_default();
                    let response = match content {
                        Value::Object(_) => content.clone(),
                        Value::String(text) => json!({"content": text}),
                        other => json!({"content": other}),
                    };
                    let mut value = json!({
                        "functionResponse": {"name": resolved_name, "response": response}
                    });
                    if is_error == &Some(true) {
                        value["functionResponse"]["response"]["error"] = json!(true);
                    }
                    // Used only for deterministic ordering below; Gemini does not
                    // receive this correlation field.
                    value["functionResponse"]["id"] = json!(call_id);
                    parts.push(value);
                }
                LlmContentPart::Unknown { value, .. } => parts.push(value.clone()),
                _ => {}
            }
        }
        if role == "user" && !function_response && !parts.iter().any(|p| p.get("text").is_some()) {
            parts.push(json!({"text":" "}));
        }
        push_gemini_content(&mut contents, role, function_response, parts);
    }

    for content in &mut contents {
        let Some(parts) = content.get_mut("parts").and_then(Value::as_array_mut) else {
            continue;
        };
        if parts
            .iter()
            .any(|part| part.get("functionResponse").is_some())
        {
            parts.sort_by_key(|part| {
                part.get("functionResponse")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                    .and_then(|id| calls.get(id))
                    .map(|(_, index)| *index)
                    .unwrap_or(usize::MAX)
            });
            for part in parts {
                if let Some(response) = part
                    .get_mut("functionResponse")
                    .and_then(Value::as_object_mut)
                {
                    response.remove("id");
                }
            }
        }
    }
    if contents.is_empty() {
        contents.push(json!({"role":"user", "parts":[{"text":" "}]}));
    }

    let mut output = json!({"contents": contents});
    let mut system = request
        .system
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            LlmContentPart::Text { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    system.extend(inline_system);
    if !system.is_empty() {
        output["systemInstruction"] = json!({"parts":[{"text":system.join("\n")}]});
    }

    if !request.tools.is_empty() {
        let declarations = request
            .tools
            .iter()
            .filter(|tool| tool.kind == "function")
            .map(|tool| {
                let mut declaration = json!({
                    "name": tool.name,
                    "parameters": super::gemini_native::normalize_gemini_schema(&tool.input_schema),
                });
                if let Some(description) = &tool.description {
                    declaration["description"] = json!(description);
                }
                declaration
            })
            .collect::<Vec<_>>();
        if !declarations.is_empty() {
            output["tools"] = json!([{"functionDeclarations": declarations}]);
        }
    }
    if let Some(choice) = &request.tool_choice {
        let mode = match choice.mode {
            LlmToolChoiceMode::None => "NONE",
            LlmToolChoiceMode::Required | LlmToolChoiceMode::Tool => "ANY",
            LlmToolChoiceMode::Auto => "AUTO",
        };
        let mut config = json!({"mode": mode});
        if choice.mode == LlmToolChoiceMode::Tool {
            if let Some(name) = &choice.name {
                config["allowedFunctionNames"] = json!([name]);
            }
        }
        output["toolConfig"] = json!({"functionCallingConfig": config});
    }
    if let Some(config) = render_gemini_generation(request, upstream_model) {
        output["generationConfig"] = config;
    }
    for (extension, field) in [
        ("google.cached_content", "cachedContent"),
        ("google.safety_settings", "safetySettings"),
        ("google.labels", "labels"),
    ] {
        if let Some(value) = request.extensions.get(extension) {
            output[field] = value.clone();
        }
    }
    if output.get("cachedContent").is_some() {
        let object = output.as_object_mut().expect("Gemini request is an object");
        object.remove("systemInstruction");
        object.remove("tools");
        object.remove("toolConfig");
    }
    Ok(Bytes::from(serde_json::to_vec(&output)?))
}

fn push_gemini_content(
    contents: &mut Vec<Value>,
    role: &str,
    function_response: bool,
    parts: Vec<Value>,
) {
    if parts.is_empty() {
        return;
    }
    let can_merge = contents.last().is_some_and(|last| {
        last.get("role").and_then(Value::as_str) == Some(role)
            && last
                .get("parts")
                .and_then(Value::as_array)
                .is_some_and(|parts| {
                    parts
                        .iter()
                        .any(|part| part.get("functionResponse").is_some())
                        == function_response
                })
    });
    if can_merge {
        contents
            .last_mut()
            .and_then(|content| content.get_mut("parts"))
            .and_then(Value::as_array_mut)
            .expect("checked Gemini content parts")
            .extend(parts);
    } else {
        contents.push(json!({"role":role, "parts":parts}));
    }
}

fn render_gemini_image(source: &LlmImageSource, media_type: Option<&str>) -> Result<Value> {
    match source {
        LlmImageSource::Data { data } => {
            if let Some(raw) = data.strip_prefix("data:") {
                let (meta, payload) = raw.split_once(',').context("invalid image data URL")?;
                let (mime, encoding) = meta.split_once(';').context("invalid image data URL")?;
                anyhow::ensure!(
                    encoding.eq_ignore_ascii_case("base64"),
                    "image data URL must be base64"
                );
                Ok(json!({"inlineData":{"mimeType":normalize_mime(mime),"data":payload}}))
            } else {
                Ok(
                    json!({"inlineData":{"mimeType":media_type.unwrap_or("application/octet-stream"),"data":data}}),
                )
            }
        }
        LlmImageSource::File { uri } if uri.starts_with("gs://") => Ok(json!({
            "fileData":{"mimeType":media_type.or_else(|| mime_from_uri(uri)).context("gs:// media needs a MIME type")?,"fileUri":uri}
        })),
        LlmImageSource::Url { url } | LlmImageSource::File { uri: url } => {
            anyhow::bail!("native Gemini accepts data: or gs:// media URLs, got {url}")
        }
    }
}

fn normalize_mime(mime: &str) -> &str {
    if mime == "image/jpg" {
        "image/jpeg"
    } else {
        mime
    }
}

fn mime_from_uri(uri: &str) -> Option<&'static str> {
    match uri.rsplit_once('.')?.1.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "pdf" => Some("application/pdf"),
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "mp4" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        "webm" => Some("video/webm"),
        "txt" => Some("text/plain"),
        _ => None,
    }
}

fn render_gemini_generation(request: &LlmRequest, model: &str) -> Option<Value> {
    let generation = &request.generation;
    let mut config = serde_json::Map::new();
    for (key, value) in [
        ("temperature", generation.temperature.map(Value::from)),
        ("topP", generation.top_p.map(Value::from)),
        ("topK", generation.top_k.map(Value::from)),
        (
            "frequencyPenalty",
            generation.frequency_penalty.map(Value::from),
        ),
        (
            "presencePenalty",
            generation.presence_penalty.map(Value::from),
        ),
        ("seed", generation.seed.map(Value::from)),
        (
            "candidateCount",
            generation.candidate_count.map(Value::from),
        ),
        (
            "maxOutputTokens",
            generation.max_output_tokens.map(Value::from),
        ),
    ] {
        if let Some(value) = value {
            config.insert(key.into(), value);
        }
    }
    if !generation.stop_sequences.is_empty() {
        config.insert("stopSequences".into(), json!(generation.stop_sequences));
    }
    if let Some(format) = &request.response_format {
        if format.kind == "json_object" || format.kind.contains("json") || format.schema.is_some() {
            config.insert("responseMimeType".into(), json!("application/json"));
        }
        if let Some(schema) = &format.schema {
            config.insert(
                "responseSchema".into(),
                super::gemini_native::normalize_gemini_schema(schema),
            );
        }
    }
    let thinking = if let Some(budget) = generation.thinking_budget {
        Some(json!({"thinkingBudget":budget,"includeThoughts":true}))
    } else {
        generation.reasoning_effort.as_ref().and_then(|value| {
            let effort = value.as_str()?;
            if model.contains("gemini-3") {
                (effort != "none").then(|| json!({"thinkingLevel":effort,"includeThoughts":true}))
            } else {
                let budget = match effort {
                    "none" => return None,
                    "minimal" | "low" => 1024,
                    "medium" => 2048,
                    "high" => 4096,
                    "xhigh" => 8192,
                    "max" => 16384,
                    _ => return None,
                };
                Some(json!({"thinkingBudget":budget,"includeThoughts":true}))
            }
        })
    };
    if let Some(thinking) = thinking {
        config.insert("thinkingConfig".into(), thinking);
    }
    (!config.is_empty()).then_some(Value::Object(config))
}

pub fn response_to_wire(
    semantic: &LlmResponseEventPayload,
    target: LlmProtocol,
    client_model: &str,
) -> Result<Bytes> {
    let value = match target {
        LlmProtocol::ChatCompletions => render_chat_response(&semantic.response, client_model),
        LlmProtocol::Messages => render_messages_response(&semantic.response, client_model),
        LlmProtocol::Responses => render_responses_response(&semantic.response, client_model),
        LlmProtocol::Gemini => render_gemini_response(&semantic.response, client_model),
        LlmProtocol::Unknown => anyhow::bail!("cannot render an unknown LLM response protocol"),
    };
    Ok(Bytes::from(serde_json::to_vec(&value)?))
}

fn render_chat_response(response: &LlmResponse, fallback_model: &str) -> Value {
    let id = response
        .id
        .clone()
        .unwrap_or_else(|| format!("chatcmpl_{}", chrono::Utc::now().timestamp_millis()));
    let choices = response
        .candidates
        .iter()
        .map(|candidate| {
            let mut message = json!({"role":"assistant"});
            let text = candidate_text(candidate);
            if !text.is_empty() || !candidate_has_tools(candidate) {
                message["content"] = json!(text);
            }
            let reasoning = candidate_reasoning(candidate);
            if !reasoning.is_empty() {
                message["reasoning_content"] = json!(reasoning);
            }
            let tool_calls = candidate_tool_calls(candidate, &id);
            if !tool_calls.is_empty() {
                message["tool_calls"] = Value::Array(tool_calls);
            }
            json!({
                "index": candidate.index,
                "message": message,
                "finish_reason": chat_finish_reason(candidate.finish_reason.as_deref(), candidate_has_tools(candidate)),
            })
        })
        .collect::<Vec<_>>();
    let choices = if choices.is_empty() {
        vec![json!({"index":0,"message":{"role":"assistant","content":""},"finish_reason":"stop"})]
    } else {
        choices
    };
    let mut value = json!({
        "id":id,
        "object":"chat.completion",
        "created":chrono::Utc::now().timestamp(),
        "model":fallback_model,
        "choices":choices,
    });
    if let Some(usage) = &response.usage {
        value["usage"] = render_chat_usage(usage);
    }
    value
}

fn render_messages_response(response: &LlmResponse, fallback_model: &str) -> Value {
    let candidate = response.candidates.first();
    let content = candidate
        .map(|candidate| {
            candidate
                .message
                .parts
                .iter()
                .filter_map(|part| match part {
                    LlmContentPart::Text { text, .. } if !text.is_empty() => {
                        Some(json!({"type":"text","text":text}))
                    }
                    LlmContentPart::Reasoning { text, signature } => {
                        let mut block = json!({"type":"thinking","thinking":text.as_deref().unwrap_or_default()});
                        if let Some(signature) = signature {
                            block["signature"] = json!(signature);
                        }
                        Some(block)
                    }
                    LlmContentPart::ToolCall { id, name, arguments, .. } => Some(json!({
                        "type":"tool_use","id":id,"name":name,"input":arguments
                    })),
                    LlmContentPart::Unknown { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let usage = response.usage.as_ref().cloned().unwrap_or_default();
    json!({
        "id":response.id.as_deref().unwrap_or("msg_proxy"),
        "type":"message",
        "role":"assistant",
        "model":fallback_model,
        "content":content,
        "stop_reason":anthropic_finish_reason(candidate.and_then(|c| c.finish_reason.as_deref()), candidate.is_some_and(candidate_has_tools)),
        "stop_sequence":null,
        "usage":{
            "input_tokens":usage.input_tokens,
            "output_tokens":usage.output_tokens,
            "cache_read_input_tokens":usage.cache_read_tokens,
            "cache_creation_input_tokens":usage.cache_write_tokens,
        }
    })
}

fn render_responses_response(response: &LlmResponse, fallback_model: &str) -> Value {
    let suffix = response
        .id
        .as_deref()
        .map(safe_id_suffix)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "proxy".into());
    let response_id = format!("resp_{suffix}");
    let mut output = Vec::new();
    let mut status = "completed";
    for candidate in &response.candidates {
        if candidate
            .finish_reason
            .as_deref()
            .is_some_and(|reason| matches!(reason, "length" | "max_tokens" | "MAX_TOKENS"))
        {
            status = "incomplete";
        }
        let text = candidate_text(candidate);
        if !text.is_empty() {
            output.push(json!({
                "type":"message",
                "id":format!("msg_{suffix}_{}", candidate.index),
                "role":"assistant",
                "status":"completed",
                "content":[{"type":"output_text","text":text,"annotations":[]}],
            }));
        }
        for (part_index, part) in candidate.message.parts.iter().enumerate() {
            match part {
                LlmContentPart::Reasoning { text, signature } => {
                    let mut item = json!({
                        "type":"reasoning",
                        "id":format!("rs_{suffix}_{part_index}"),
                        "summary":text.as_ref().map(|text| vec![json!({"type":"summary_text","text":text})]).unwrap_or_default(),
                    });
                    if let Some(signature) = signature {
                        item["encrypted_content"] = json!(signature);
                    }
                    output.push(item);
                }
                LlmContentPart::ToolCall {
                    id,
                    name,
                    arguments,
                    ..
                } => {
                    let call_id = if id.is_empty() {
                        format!("call_{suffix}_{part_index}")
                    } else {
                        id.clone()
                    };
                    output.push(json!({
                        "type":"function_call",
                        "id":call_id,
                        "call_id":call_id,
                        "name":name,
                        "arguments":serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into()),
                        "status":"completed",
                    }));
                }
                LlmContentPart::Unknown { value, .. } => output.push(value.clone()),
                _ => {}
            }
        }
    }
    let usage = response.usage.as_ref().cloned().unwrap_or_default();
    json!({
        "id":response_id,
        "object":"response",
        "created_at":chrono::Utc::now().timestamp(),
        "status":status,
        "model":fallback_model,
        "output":output,
        "usage":{
            "input_tokens":usage.input_tokens,
            "output_tokens":usage.output_tokens,
            "total_tokens":usage.total_tokens,
            "input_tokens_details":{"cached_tokens":usage.cache_read_tokens},
            "output_tokens_details":{"reasoning_tokens":usage.reasoning_tokens},
        }
    })
}

fn render_gemini_response(response: &LlmResponse, fallback_model: &str) -> Value {
    let candidates = response
        .candidates
        .iter()
        .map(|candidate| {
            let parts = candidate
                .message
                .parts
                .iter()
                .filter_map(|part| match part {
                    LlmContentPart::Text { text, .. } => Some(json!({"text":text})),
                    LlmContentPart::Reasoning { text, signature } => {
                        let mut value =
                            json!({"text":text.as_deref().unwrap_or_default(),"thought":true});
                        if let Some(signature) = signature {
                            value["thoughtSignature"] = json!(signature);
                        }
                        Some(value)
                    }
                    LlmContentPart::ToolCall {
                        name,
                        arguments,
                        signature,
                        ..
                    } => {
                        let mut value = json!({"functionCall":{"name":name,"args":arguments}});
                        if let Some(signature) = signature {
                            value["thoughtSignature"] = json!(signature);
                        }
                        Some(value)
                    }
                    LlmContentPart::Unknown { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            json!({
                "index":candidate.index,
                "content":{"role":"model","parts":parts},
                "finishReason":gemini_finish_reason(candidate.finish_reason.as_deref()),
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "responseId":response.id,
        "modelVersion":fallback_model,
        "candidates":candidates,
    });
    if let Some(usage) = &response.usage {
        value["usageMetadata"] = json!({
            "promptTokenCount":usage.input_tokens,
            "candidatesTokenCount":usage.output_tokens,
            "totalTokenCount":usage.total_tokens,
            "cachedContentTokenCount":usage.cache_read_tokens,
            "thoughtsTokenCount":usage.reasoning_tokens,
        });
    }
    remove_nulls(&mut value);
    value
}

fn candidate_text(candidate: &LlmCandidate) -> String {
    candidate
        .message
        .parts
        .iter()
        .filter_map(|part| match part {
            LlmContentPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

fn candidate_reasoning(candidate: &LlmCandidate) -> String {
    candidate
        .message
        .parts
        .iter()
        .filter_map(|part| match part {
            LlmContentPart::Reasoning {
                text: Some(text), ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

fn candidate_has_tools(candidate: &LlmCandidate) -> bool {
    candidate
        .message
        .parts
        .iter()
        .any(|part| matches!(part, LlmContentPart::ToolCall { .. }))
}

fn candidate_tool_calls(candidate: &LlmCandidate, response_id: &str) -> Vec<Value> {
    candidate
        .message
        .parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| match part {
            LlmContentPart::ToolCall { id, name, arguments, signature } => {
                let base = if id.is_empty() { format!("call_{response_id}_{index}") } else { id.clone() };
                let id = signature.as_ref().map(|signature| format!("{base}__thought__{signature}")).unwrap_or(base);
                Some(json!({
                    "id":id,"type":"function",
                    "function":{"name":name,"arguments":serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into())}
                }))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn render_chat_usage(usage: &LlmUsage) -> Value {
    json!({
        "prompt_tokens":usage.input_tokens,
        "completion_tokens":usage.output_tokens,
        "total_tokens":usage.total_tokens,
        "prompt_tokens_details":{"cached_tokens":usage.cache_read_tokens},
        "completion_tokens_details":{"reasoning_tokens":usage.reasoning_tokens},
    })
}

pub(super) fn chat_finish_reason(reason: Option<&str>, has_tools: bool) -> &'static str {
    match reason {
        Some("length" | "max_tokens" | "MAX_TOKENS") => "length",
        Some("content_filter" | "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT") => {
            "content_filter"
        }
        Some("tool_calls" | "tool_use") => "tool_calls",
        _ if has_tools => "tool_calls",
        _ => "stop",
    }
}

pub(super) fn anthropic_finish_reason(reason: Option<&str>, has_tools: bool) -> &'static str {
    match reason {
        Some("length" | "max_tokens" | "MAX_TOKENS") => "max_tokens",
        Some("tool_calls" | "tool_use") => "tool_use",
        _ if has_tools => "tool_use",
        _ => "end_turn",
    }
}

pub(super) fn gemini_finish_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("length" | "max_tokens") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        _ => "STOP",
    }
}

pub(super) fn safe_id_suffix(raw: &str) -> String {
    raw.chars()
        .filter(char::is_ascii_alphanumeric)
        .take(24)
        .collect()
}

fn render_chat_completions(
    request: &LlmRequest,
    source: &LlmProtocol,
    upstream_model: &str,
) -> Value {
    let prompt_cache = supports_prompt_cache_breakpoint(upstream_model);
    let mut messages = Vec::new();
    for message in &request.system {
        render_message(message, source, prompt_cache, &mut messages);
    }
    for message in &request.messages {
        render_message(message, source, prompt_cache, &mut messages);
    }

    let mut output = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": request.stream,
    });
    if request.stream {
        output["stream_options"] = json!({"include_usage":true});
    }
    let generation = &request.generation;
    copy_option(&mut output, "temperature", generation.temperature);
    copy_option(&mut output, "top_p", generation.top_p);
    copy_option(&mut output, "seed", generation.seed);
    copy_option(
        &mut output,
        "frequency_penalty",
        generation.frequency_penalty,
    );
    copy_option(&mut output, "presence_penalty", generation.presence_penalty);
    if let Some(count) = generation.candidate_count {
        output["n"] = json!(count);
    }
    if let Some(max) = generation.max_output_tokens {
        let key = if *source == LlmProtocol::Messages {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        output[key] = json!(max);
    } else if *source == LlmProtocol::Messages {
        output["max_completion_tokens"] = json!(1024);
    }
    if !generation.stop_sequences.is_empty() {
        output["stop"] = json!(generation.stop_sequences);
    }

    if !request.tools.is_empty() {
        output["tools"] = Value::Array(
            request
                .tools
                .iter()
                .filter(|tool| tool.kind == "function")
                .map(|tool| {
                    let mut function = json!({
                        "name":tool.name,
                        "description":tool.description,
                        "parameters":tool.input_schema,
                    });
                    if let Some(strict) = tool.strict {
                        function["strict"] = json!(strict);
                    }
                    json!({"type":"function","function":function})
                })
                .collect(),
        );
    }
    if let Some(choice) = &request.tool_choice {
        output["tool_choice"] = match choice.mode {
            LlmToolChoiceMode::Auto => json!("auto"),
            LlmToolChoiceMode::None => json!("none"),
            LlmToolChoiceMode::Required => json!("required"),
            LlmToolChoiceMode::Tool => json!({
                "type":"function",
                "function":{"name":choice.name.clone().unwrap_or_default()}
            }),
        };
        if let Some(parallel) = choice.parallel {
            output["parallel_tool_calls"] = json!(parallel);
        }
    } else if let Some(parallel) = request
        .extensions
        .get("openai.parallel_tool_calls")
        .and_then(Value::as_bool)
    {
        output["parallel_tool_calls"] = json!(parallel);
    }

    if let Some(format) = &request.response_format {
        if let Some(schema) = &format.schema {
            output["response_format"] = json!({
                "type":"json_schema",
                "json_schema":{
                    "name":format.name.clone().unwrap_or_else(|| "structured_output".into()),
                    "schema":schema,
                    "strict":format.strict,
                }
            });
            remove_nulls(&mut output["response_format"]);
        } else if format.kind == "json_object" || format.kind == "text" {
            output["response_format"] = json!({"type":format.kind});
        }
    }

    if *source == LlmProtocol::Messages {
        if let Some(effort) = anthropic_reasoning_effort(request, upstream_model) {
            output["reasoning_effort"] = Value::String(effort);
        }
        if let Some(user) = request
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("user_id"))
        {
            output["user"] = user.clone();
        }
    }
    output
}

fn render_message(
    message: &LlmMessage,
    source: &LlmProtocol,
    prompt_cache: bool,
    output: &mut Vec<Value>,
) {
    if message.role == LlmRole::Tool {
        for part in &message.parts {
            if let LlmContentPart::ToolResult {
                call_id,
                name,
                content,
                cache_control,
                ..
            } = part
            {
                let mut tool = json!({
                    "role":"tool",
                    "tool_call_id":call_id,
                    "content":render_tool_result(content, cache_control.as_ref(), source, prompt_cache),
                });
                if let Some(name) = name {
                    tool["name"] = Value::String(name.clone());
                }
                output.push(tool);
            }
        }
        return;
    }

    let mut regular_parts = Vec::new();
    let mut tool_calls = Vec::new();
    for part in &message.parts {
        match part {
            LlmContentPart::ToolResult {
                call_id,
                name,
                content,
                cache_control,
                ..
            } => {
                flush_regular_message(message, source, &mut regular_parts, &mut tool_calls, output);
                let mut tool = json!({
                    "role":"tool",
                    "tool_call_id":call_id,
                    "content":render_tool_result(content, cache_control.as_ref(), source, prompt_cache),
                });
                if let Some(name) = name {
                    tool["name"] = Value::String(name.clone());
                }
                output.push(tool);
            }
            LlmContentPart::ToolCall {
                id,
                name,
                arguments,
                signature,
            } => {
                let id = signature
                    .as_ref()
                    .map(|signature| format!("{id}__thought__{signature}"))
                    .unwrap_or_else(|| id.clone());
                tool_calls.push(json!({
                    "id":id,
                    "type":"function",
                    "function":{
                        "name":name,
                        "arguments":serde_json::to_string(arguments).unwrap_or_else(|_| "{}".into())
                    }
                }));
            }
            other => {
                if let Some(rendered) = render_content_part(other, source, prompt_cache) {
                    regular_parts.push(rendered);
                }
            }
        }
    }
    flush_regular_message(message, source, &mut regular_parts, &mut tool_calls, output);
}

fn flush_regular_message(
    message: &LlmMessage,
    source: &LlmProtocol,
    regular_parts: &mut Vec<Value>,
    tool_calls: &mut Vec<Value>,
    output: &mut Vec<Value>,
) {
    if regular_parts.is_empty() && tool_calls.is_empty() {
        return;
    }
    let role = match message.role {
        LlmRole::System | LlmRole::Developer => "system",
        LlmRole::Assistant => "assistant",
        LlmRole::Tool => "tool",
        LlmRole::User => "user",
    };
    if *source == LlmProtocol::Responses && !regular_parts.is_empty() {
        for content in std::mem::take(regular_parts) {
            output.push(json!({"role":role,"content":content}));
        }
        if tool_calls.is_empty() {
            return;
        }
    }
    let mut rendered = json!({"role":role});
    if !regular_parts.is_empty() {
        rendered["content"] = Value::Array(std::mem::take(regular_parts));
    } else if !tool_calls.is_empty() && *source == LlmProtocol::Responses {
        rendered["content"] = Value::Null;
    }
    if !tool_calls.is_empty() {
        rendered["tool_calls"] = Value::Array(std::mem::take(tool_calls));
    }
    if let Some(name) = &message.name {
        rendered["name"] = Value::String(name.clone());
    }
    output.push(rendered);
}

fn render_content_part(
    part: &LlmContentPart,
    source: &LlmProtocol,
    prompt_cache: bool,
) -> Option<Value> {
    match part {
        LlmContentPart::Text {
            text,
            cache_control,
        } => {
            if *source == LlmProtocol::Responses {
                return Some(Value::String(text.clone()));
            }
            let mut value = json!({"type":"text","text":text});
            if prompt_cache && cache_control.is_some() {
                value["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
            }
            Some(value)
        }
        LlmContentPart::Image {
            source: image,
            media_type,
            detail,
        } => {
            let url = match image {
                LlmImageSource::Url { url } => url.clone(),
                LlmImageSource::File { uri } => uri.clone(),
                LlmImageSource::Data { data } if data.starts_with("data:") => data.clone(),
                LlmImageSource::Data { data } => format!(
                    "data:{};base64,{data}",
                    media_type.as_deref().unwrap_or("application/octet-stream")
                ),
            };
            let mut image_url = json!({"url":url});
            if let Some(detail) = detail {
                image_url["detail"] = Value::String(detail.clone());
            }
            Some(json!({"type":"image_url","image_url":image_url}))
        }
        // Reasoning is retained in the Chronicle IR but Chat Completions has no
        // lossless replay field for provider-signed thinking blocks.
        LlmContentPart::Reasoning { .. } => None,
        LlmContentPart::Unknown { value, .. } => Some(value.clone()),
        _ => None,
    }
}

fn render_tool_result(
    content: &Value,
    outer_cache: Option<&Value>,
    source: &LlmProtocol,
    prompt_cache: bool,
) -> Value {
    if *source == LlmProtocol::Responses {
        return content.clone();
    }
    let outer_cache = prompt_cache && outer_cache.is_some_and(Value::is_object);
    match content {
        Value::String(text) if outer_cache => json!([{
            "type":"text",
            "text":text,
            "prompt_cache_breakpoint":{"mode":"explicit"}
        }]),
        Value::Array(parts) => {
            let trailing_cache = outer_cache
                || (prompt_cache
                    && parts.iter().any(|part| {
                        part.get("type").and_then(Value::as_str) != Some("text")
                            && part.get("cache_control").is_some_and(Value::is_object)
                    }));
            let mut rendered = parts
                .iter()
                .filter_map(|part| parse_tool_result_part(part, prompt_cache))
                .collect::<Vec<_>>();
            if trailing_cache {
                if let Some(last) = rendered.last_mut() {
                    last["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
                } else {
                    rendered.push(json!({
                        "type":"text",
                        "text":"",
                        "prompt_cache_breakpoint":{"mode":"explicit"}
                    }));
                }
            }
            Value::Array(rendered)
        }
        value => value.clone(),
    }
}

fn parse_tool_result_part(part: &Value, prompt_cache: bool) -> Option<Value> {
    let text = part.get("text").and_then(Value::as_str)?;
    let mut rendered = json!({"type":"text","text":text});
    if prompt_cache && part.get("cache_control").is_some() {
        rendered["prompt_cache_breakpoint"] = json!({"mode":"explicit"});
    }
    Some(rendered)
}

fn anthropic_reasoning_effort(request: &LlmRequest, model: &str) -> Option<String> {
    let thinking = request.extensions.get("anthropic.thinking")?;
    if thinking.get("type").and_then(Value::as_str) != Some("adaptive") {
        return None;
    }
    if !request.tools.is_empty() && !supports_reasoning_with_tools(model) {
        return Some("none".into());
    }
    Some(
        request
            .extensions
            .get("anthropic.output_config")
            .and_then(|value| value.get("effort"))
            .and_then(Value::as_str)
            .unwrap_or("high")
            .into(),
    )
}

fn supports_prompt_cache_breakpoint(model: &str) -> bool {
    model
        .strip_prefix("gpt-")
        .and_then(|model| model.split('-').next())
        .and_then(|version| version.split_once('.'))
        .and_then(|(major, minor)| Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?)))
        .is_some_and(|version| version >= (5, 6))
}

fn supports_reasoning_with_tools(model: &str) -> bool {
    !model.starts_with("gpt-")
        || model == "gpt-5"
        || model.starts_with("gpt-5-")
        || model.starts_with("gpt-5.1")
        || model.starts_with("gpt-5.2")
}

fn copy_option<T: serde::Serialize>(output: &mut Value, key: &str, value: Option<T>) {
    if let Some(value) = value {
        output[key] = serde_json::to_value(value).unwrap_or(Value::Null);
    }
}

fn remove_nulls(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| !value.is_null());
        for value in object.values_mut() {
            remove_nulls(value);
        }
    }
}
