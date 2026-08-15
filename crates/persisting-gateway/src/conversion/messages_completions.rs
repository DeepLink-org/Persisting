//! Anthropic Messages ↔ OpenAI Chat Completions.
//!
//! The wire mapping follows agentgateway's `agent-llm` conversion contract. Keep
//! this module lossless for agent-critical semantics (tools, tool results and
//! multimodal content); unsupported server-side tools are the only intentional
//! omission because Chat Completions has no equivalent execution model.

use anyhow::{Context, Result};
use bytes::Bytes;
use serde_json::{json, Value};

/// Convert Anthropic `/v1/messages` JSON body to OpenAI `/v1/chat/completions`.
pub fn messages_request_to_completions(body: &Bytes, upstream_model: &str) -> Result<Bytes> {
    let v: Value = serde_json::from_slice(body).context("parse messages request")?;
    let obj = v
        .as_object()
        .context("messages request must be a JSON object")?;
    let supports_prompt_cache = supports_prompt_cache_breakpoint(upstream_model);
    let mut out_messages = Vec::new();
    if let Some(system) = obj.get("system") {
        append_system_message(system, supports_prompt_cache, &mut out_messages);
    }

    if let Some(msgs) = obj.get("messages").and_then(Value::as_array) {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            append_message(
                role,
                msg.get("content").unwrap_or(&Value::Null),
                supports_prompt_cache,
                &mut out_messages,
            );
        }
    }

    let stream = obj.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let mut out = json!({
        "messages": out_messages,
        "model": upstream_model,
        "max_completion_tokens": obj.get("max_tokens").cloned().unwrap_or(json!(1024)),
        "stream": stream,
    });

    copy_non_null(obj, &mut out, "temperature", "temperature");
    copy_non_null(obj, &mut out, "top_p", "top_p");
    if let Some(stop) = obj.get("stop_sequences").filter(|v| !is_empty_array(v)) {
        out["stop"] = stop.clone();
    }
    if stream {
        out["stream_options"] = json!({"include_usage": true});
    }
    let mut has_function_tools = false;
    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        let converted = tools.iter().filter_map(convert_tool).collect::<Vec<_>>();
        if !converted.is_empty() {
            has_function_tools = true;
            out["tools"] = json!(converted);
        }
    }
    if let Some(choice) = obj.get("tool_choice") {
        let (tool_choice, parallel) = convert_tool_choice(choice);
        if let Some(tool_choice) = tool_choice {
            out["tool_choice"] = tool_choice;
        }
        if let Some(parallel) = parallel {
            out["parallel_tool_calls"] = json!(parallel);
        }
    }
    if let Some(effort) = reasoning_effort(obj, upstream_model, has_function_tools) {
        out["reasoning_effort"] = json!(effort);
    }
    if let Some(format) = response_format(obj) {
        out["response_format"] = format;
    }
    if let Some(user_id) = obj
        .get("metadata")
        .and_then(|v| v.get("user_id"))
        .filter(|v| !v.is_null())
    {
        out["user"] = user_id.clone();
    }

    Ok(Bytes::from(
        serde_json::to_vec(&out).context("serialize completions request")?,
    ))
}

/// Convert OpenAI chat completion JSON to Anthropic messages response shape.
pub fn completions_response_to_messages(body: &Bytes, client_model: &str) -> Result<Bytes> {
    let v: Value = serde_json::from_slice(body).context("parse completions response")?;
    Ok(Bytes::from(
        serde_json::to_vec(&completions_value_to_messages(&v, client_model)?)
            .context("serialize messages response")?,
    ))
}

pub fn completions_value_to_messages(body: &Value, client_model: &str) -> Result<Value> {
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .context("completions response missing choices[0]")?;
    let message = choice.get("message").context("choice missing message")?;
    let text = message.get("content").and_then(Value::as_str).unwrap_or("");
    let finish = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop");
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("msg_proxy");
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let Some(function) = tool_call.get("function") else {
                continue;
            };
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .map(|arguments| {
                    serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!(arguments))
                })
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tool_call.get("id").cloned().unwrap_or(json!("call_proxy")),
                "name": function.get("name").cloned().unwrap_or(json!("")),
                "input": input,
            }));
        }
    }
    let usage = openai_usage_to_anthropic(body);

    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": client_model,
        "content": content,
        "stop_reason": openai_finish_to_anthropic(finish),
        "stop_sequence": null,
        "usage": usage,
    }))
}

fn append_system_message(system: &Value, prompt_cache: bool, messages: &mut Vec<Value>) {
    let content = match system {
        Value::String(text) if !text.is_empty() => json!(text),
        Value::Array(blocks) => json!(blocks
            .iter()
            .filter_map(|block| text_part(block, prompt_cache))
            .collect::<Vec<_>>()),
        _ => return,
    };
    if !is_empty_array(&content) {
        messages.push(json!({"role": "system", "content": content}));
    }
}

fn append_message(role: &str, content: &Value, prompt_cache: bool, messages: &mut Vec<Value>) {
    match content {
        Value::String(text) => messages.push(json!({
            "role": role,
            "content": [{"type": "text", "text": text}],
        })),
        Value::Array(blocks) => {
            if role == "assistant" {
                append_assistant_blocks(blocks, prompt_cache, messages);
                return;
            }
            let mut parts = Vec::new();
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                    flush_content_message(role, &mut parts, messages);
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": block.get("tool_use_id").cloned().unwrap_or(json!("")),
                        "content": tool_result_content(block, prompt_cache),
                    }));
                    continue;
                }
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(part) = text_part(block, prompt_cache) {
                            parts.push(part);
                        }
                    }
                    Some("image") => image_part(block).into_iter().for_each(|p| parts.push(p)),
                    _ => {}
                }
            }
            flush_content_message(role, &mut parts, messages);
        }
        _ => {}
    }
}

fn append_assistant_blocks(blocks: &[Value], prompt_cache: bool, messages: &mut Vec<Value>) {
    let mut content = Vec::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => text_part(block, prompt_cache)
                .into_iter()
                .for_each(|p| content.push(p)),
            Some("tool_use") => tool_calls.push(json!({
                "id": block.get("id").cloned().unwrap_or(json!("call_proxy")),
                "type": "function",
                "function": {
                    "name": block.get("name").cloned().unwrap_or(json!("")),
                    "arguments": serde_json::to_string(block.get("input").unwrap_or(&json!({})))
                        .unwrap_or_else(|_| "{}".to_string()),
                }
            })),
            _ => {}
        }
    }
    if content.is_empty() && tool_calls.is_empty() {
        return;
    }
    let mut message = json!({"role": "assistant"});
    if !content.is_empty() {
        message["content"] = json!(content);
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }
    messages.push(message);
}

fn flush_content_message(role: &str, parts: &mut Vec<Value>, messages: &mut Vec<Value>) {
    if !parts.is_empty() {
        messages.push(json!({"role": role, "content": std::mem::take(parts)}));
    }
}

fn text_part(block: &Value, prompt_cache: bool) -> Option<Value> {
    let text = block.get("text").and_then(Value::as_str)?;
    let mut part = json!({"type": "text", "text": text});
    if prompt_cache && block.get("cache_control").is_some_and(Value::is_object) {
        part["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
    }
    Some(part)
}

fn image_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    let url = match source.get("type").and_then(Value::as_str)? {
        "base64" => format!(
            "data:{};base64,{}",
            source.get("media_type")?.as_str()?,
            source.get("data")?.as_str()?
        ),
        "url" => source.get("url")?.as_str()?.to_string(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

fn tool_result_content(block: &Value, prompt_cache: bool) -> Value {
    let outer_cache = prompt_cache && block.get("cache_control").is_some_and(Value::is_object);
    match block.get("content") {
        Some(Value::String(text)) if outer_cache => json!([{
            "type": "text",
            "text": text,
            "prompt_cache_breakpoint": {"mode": "explicit"},
        }]),
        Some(Value::String(text)) => json!(text),
        Some(Value::Array(parts)) => {
            let mut converted = Vec::new();
            let mut trailing_cache = outer_cache;
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(text) = text_part(part, prompt_cache) {
                        converted.push(text);
                    }
                } else if prompt_cache && part.get("cache_control").is_some_and(Value::is_object) {
                    trailing_cache = true;
                }
            }
            if trailing_cache {
                if let Some(last) = converted.last_mut() {
                    last["prompt_cache_breakpoint"] = json!({"mode": "explicit"});
                } else {
                    converted.push(json!({
                        "type": "text",
                        "text": "",
                        "prompt_cache_breakpoint": {"mode": "explicit"},
                    }));
                }
            }
            json!(converted)
        }
        Some(other) => json!(other.to_string()),
        None => json!(""),
    }
}

fn supports_prompt_cache_breakpoint(model: &str) -> bool {
    model
        .strip_prefix("gpt-")
        .and_then(|model| model.split('-').next())
        .and_then(|version| version.split_once('.'))
        .and_then(|(major, minor)| Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?)))
        .is_some_and(|version| version >= (5, 6))
}

fn convert_tool(tool: &Value) -> Option<Value> {
    let name = tool.get("name")?;
    let schema = tool.get("input_schema")?;
    Some(json!({
        "type": "function",
        "function": {
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": schema,
        }
    }))
}

fn convert_tool_choice(choice: &Value) -> (Option<Value>, Option<bool>) {
    let parallel = choice
        .get("disable_parallel_tool_use")
        .and_then(Value::as_bool)
        .map(|disabled| !disabled);
    let converted = match choice.get("type").and_then(Value::as_str) {
        Some("auto") => Some(json!("auto")),
        Some("any") => Some(json!("required")),
        Some("none") => Some(json!("none")),
        Some("tool") => Some(json!({
            "type": "function",
            "function": {"name": choice.get("name").cloned().unwrap_or(json!(""))},
        })),
        _ => None,
    };
    (converted, parallel)
}

fn reasoning_effort<'a>(
    obj: &'a serde_json::Map<String, Value>,
    model: &str,
    has_function_tools: bool,
) -> Option<&'a str> {
    let adaptive = obj
        .get("thinking")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        == Some("adaptive");
    adaptive.then(|| {
        if has_function_tools && !supports_reasoning_with_tools(model) {
            return "none";
        }
        obj.get("output_config")
            .and_then(|v| v.get("effort"))
            .and_then(Value::as_str)
            .unwrap_or("high")
    })
}

fn supports_reasoning_with_tools(model: &str) -> bool {
    !model.starts_with("gpt-")
        || model == "gpt-5"
        || model.starts_with("gpt-5-")
        || model.starts_with("gpt-5.1")
        || model.starts_with("gpt-5.2")
}

fn response_format(obj: &serde_json::Map<String, Value>) -> Option<Value> {
    let format = obj.get("output_config")?.get("format")?;
    let schema = format.get("schema")?;
    Some(json!({
        "type": "json_schema",
        "json_schema": {"name": "structured_output", "schema": schema},
    }))
}

fn copy_non_null(
    source: &serde_json::Map<String, Value>,
    target: &mut Value,
    source_key: &str,
    target_key: &str,
) {
    if let Some(value) = source.get(source_key).filter(|v| !v.is_null()) {
        target[target_key] = value.clone();
    }
}

fn is_empty_array(value: &Value) -> bool {
    value.as_array().is_some_and(Vec::is_empty)
}

fn openai_finish_to_anthropic(finish: &str) -> &'static str {
    match finish {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn openai_usage_to_anthropic(body: &Value) -> Value {
    let usage = body.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prompt_details = usage.and_then(|u| u.get("prompt_tokens_details"));
    let cache_read_tokens = prompt_details
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let cache_creation_tokens = prompt_details
        .and_then(|details| details.get("cache_write_tokens"))
        .and_then(Value::as_u64);
    let mut out = json!({
        "input_tokens": prompt_tokens
            .saturating_sub(cache_read_tokens.unwrap_or(0))
            .saturating_sub(cache_creation_tokens.unwrap_or(0)),
        "output_tokens": usage
            .and_then(|u| u.get("completion_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    });
    if let Some(service_tier) = body.get("service_tier").filter(|v| !v.is_null()) {
        out["service_tier"] = service_tier.clone();
    }
    if let Some(tokens) = cache_creation_tokens {
        out["cache_creation_input_tokens"] = json!(tokens);
    }
    if let Some(tokens) = cache_read_tokens {
        out["cache_read_input_tokens"] = json!(tokens);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_messages_request_to_completions() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/requests/messages/basic.json"
        ));
        let out = messages_request_to_completions(&body, "deepseek-chat").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "deepseek-chat");
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"][0]["type"], "text");
        assert_eq!(v["messages"][0]["content"][0]["text"], "Hello, world");
        assert!(v.get("system").is_none());
    }

    #[test]
    fn basic_completions_response_to_messages() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/response/completions/basic.json"
        ));
        let out = completions_response_to_messages(&body, "claude-test").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["model"], "claude-test");
        assert_eq!(v["content"][0]["type"], "text");
        assert!(v["content"][0]["text"].as_str().unwrap().contains("Sorry"));
        assert_eq!(v["stop_reason"], "end_turn");
        assert_eq!(v["usage"]["input_tokens"], 17);
        assert_eq!(v["usage"]["output_tokens"], 23);
    }

    #[test]
    fn system_string_preserved() {
        let body = Bytes::from_static(br#"{"model":"m","max_tokens":1,"system":"sys","messages":[{"role":"user","content":"hi"}]}"#);
        let out = messages_request_to_completions(&body, "upstream").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "sys");
        assert_eq!(v["messages"][1]["content"][0]["text"], "hi");
    }
}
