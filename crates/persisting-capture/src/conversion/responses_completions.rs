//! OpenAI Responses API ↔ Chat Completions (Codex / DeepSeek subset).

use anyhow::{Context, Result};
use bytes::Bytes;
use serde_json::{json, Value};

use super::tool_call::{
    chat_tool_call_from_input_item, is_tool_call_input_type, is_tool_call_output_type,
    unquote_chat_tool_arguments,
};

/// Convert `/v1/responses` JSON body to `/v1/chat/completions`.
pub fn responses_request_to_completions(
    body: &Bytes,
    upstream_model: &str,
    reasoning_cache: Option<&crate::proxy::deepseek_reasoning::ReasoningCacheHandle>,
) -> Result<Bytes> {
    let mut v: Value = serde_json::from_slice(body).context("parse responses request")?;
    let obj = v
        .as_object_mut()
        .context("responses request must be a JSON object")?;

    let mut messages = Vec::new();

    if let Some(instructions) = obj.remove("instructions").and_then(|i| value_to_text(&i)) {
        if !instructions.is_empty() {
            messages.push(json!({"role": "system", "content": instructions}));
        }
    }

    if let Some(input) = obj.remove("input") {
        append_input_as_messages(&input, &mut messages);
    }

    let stream = obj.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);

    let mut out = json!({
        "model": upstream_model,
        "messages": messages,
        "stream": stream,
    });

    if stream {
        out["stream_options"] = json!({"include_usage": true});
    }

    if let Some(t) = obj.get("temperature").filter(|v| !v.is_null()) {
        out["temperature"] = t.clone();
    }
    if let Some(t) = obj.get("top_p").filter(|v| !v.is_null()) {
        out["top_p"] = t.clone();
    }
    if let Some(n) = obj.get("max_output_tokens").filter(|v| !v.is_null()) {
        out["max_tokens"] = n.clone();
    }
    for key in ["tools", "tool_choice", "parallel_tool_calls"] {
        if let Some(v) = obj.get(key).filter(|v| !v.is_null()) {
            if key == "tools" {
                out[key] = normalize_responses_tools(v);
            } else {
                out[key] = v.clone();
            }
        }
    }

    if let Some(cache) = reasoning_cache {
        if let Some(msgs) = out.get_mut("messages").and_then(|m| m.as_array_mut()) {
            cache.apply_to_messages(msgs);
        }
    }

    Ok(Bytes::from(
        serde_json::to_vec(&out).context("serialize completions request")?,
    ))
}

/// Convert chat completion JSON to OpenAI Responses response shape.
pub fn completions_response_to_responses(body: &Bytes, client_model: &str) -> Result<Bytes> {
    let v: Value = serde_json::from_slice(body).context("parse completions response")?;
    Ok(Bytes::from(
        serde_json::to_vec(&completions_value_to_responses(&v, client_model)?)
            .context("serialize responses body")?,
    ))
}

pub fn completions_value_to_responses(body: &Value, client_model: &str) -> Result<Value> {
    let choice = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .context("completions response missing choices[0]")?;
    let message = choice.get("message").context("choice missing message")?;
    let text = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let finish = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop");
    let response_id = format!("resp_{}", body_id_suffix(body));
    let message_id = format!("msg_{}", body_id_suffix(body));

    let status = match finish {
        "length" => "incomplete",
        "content_filter" => "failed",
        _ => "completed",
    };

    let usage = body.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(input_tokens + output_tokens);

    let mut output = Vec::new();
    if !text.is_empty() {
        output.push(json!({
            "type": "message",
            "id": message_id,
            "role": "assistant",
            "status": "completed",
            "content": [{
                "type": "output_text",
                "text": text,
                "annotations": []
            }]
        }));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tool_calls {
            let call_id = tc
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("call_proxy");
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .map(unquote_chat_tool_arguments)
                .unwrap_or_else(|| "{}".to_string());
            output.push(json!({
                "type": "function_call",
                "id": call_id,
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
                "status": "completed"
            }));
        }
    }

    Ok(json!({
        "id": response_id,
        "object": "response",
        "created_at": body.get("created").cloned().unwrap_or(json!(0)),
        "status": status,
        "model": client_model,
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": total_tokens
        }
    }))
}

fn body_id_suffix(body: &Value) -> String {
    body.get("id")
        .and_then(|i| i.as_str())
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(16)
                .collect()
        })
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "proxy".to_string())
}

fn normalize_responses_tools(tools: &Value) -> Value {
    let Some(items) = tools.as_array() else {
        return tools.clone();
    };
    json!(
        items
            .iter()
            .filter_map(|tool| {
                let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if tool_type != "function" {
                    return None;
                }
                if tool.get("function").is_some() {
                    return Some(tool.clone());
                }
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").cloned().unwrap_or(json!("")),
                        "description": tool.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": tool.get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}})),
                        "strict": tool.get("strict").cloned().unwrap_or(Value::Null),
                    }
                }))
            })
            .collect::<Vec<_>>()
    )
}

fn append_input_as_messages(input: &Value, messages: &mut Vec<Value>) {
    match input {
        Value::String(s) if !s.trim().is_empty() => {
            messages.push(json!({"role": "user", "content": s}));
        }
        Value::Array(items) => {
            let mut converter = InputConverter::default();
            for item in items {
                converter.push_item(item);
            }
            converter.flush(&mut *messages);
        }
        _ => {}
    }
}

/// Batches consecutive Responses `function_call` items into one assistant message
/// (moon-bridge `convertInput` / `pendingFCBlocks` pattern).
#[derive(Default)]
struct InputConverter {
    pending_tool_calls: Vec<Value>,
    legacy_messages: Option<Vec<Value>>,
}

impl InputConverter {
    fn push_item(&mut self, item: &Value) {
        if let Some(item_type) = item.get("type").and_then(|t| t.as_str()) {
            if is_tool_call_output_type(item_type) {
                self.flush_pending_tool_calls();
                self.push_tool_result(item);
                return;
            }
            if is_tool_call_input_type(item_type) {
                self.pending_tool_calls
                    .push(chat_tool_call_from_input_item(item));
                return;
            }
            if item_type == "reasoning" {
                // Codex reasoning items are not replayed to Chat Completions upstreams.
                return;
            }
            if item_type == "message" {
                self.flush_pending_tool_calls();
                append_message_input_item(item, &mut self.pending_messages());
                return;
            }
        }

        self.flush_pending_tool_calls();
        append_legacy_input_item(item, &mut self.pending_messages());
    }

    fn flush(&mut self, messages: &mut Vec<Value>) {
        self.flush_pending_tool_calls();
        messages.extend(self.drain_pending_messages());
    }

    fn pending_messages(&mut self) -> &mut Vec<Value> {
        // Lazily allocate only when legacy path needs it.
        self.legacy_messages.get_or_insert_with(Vec::new)
    }

    fn drain_pending_messages(&mut self) -> Vec<Value> {
        self.legacy_messages.take().unwrap_or_default()
    }

    fn flush_pending_tool_calls(&mut self) {
        if self.pending_tool_calls.is_empty() {
            return;
        }
        let tool_calls = std::mem::take(&mut self.pending_tool_calls);
        self.pending_messages().push(json!({
            "role": "assistant",
            "content": null,
            "tool_calls": tool_calls
        }));
    }

    fn push_tool_result(&mut self, item: &Value) {
        let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let output = function_call_output_text(item.get("output"));
        self.pending_messages().push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": output
        }));
    }
}

fn append_legacy_input_item(item: &Value, messages: &mut Vec<Value>) {
    let role = item
        .get("role")
        .or_else(|| item.get("type").filter(|t| is_role_name(t)))
        .and_then(|r| r.as_str())
        .map(normalize_role)
        .unwrap_or("user");

    if let Some(Value::Array(parts)) = item.get("content") {
        for part in parts {
            let text = content_part_to_text(part);
            if !text.is_empty() {
                messages.push(json!({"role": role, "content": text}));
            }
        }
        return;
    }

    let text = input_item_text(item);
    if text.is_empty() {
        return;
    }

    messages.push(json!({"role": role, "content": text}));
}

fn append_message_input_item(item: &Value, messages: &mut Vec<Value>) {
    let role = item
        .get("role")
        .and_then(|r| r.as_str())
        .map(normalize_role)
        .unwrap_or("user");
    if let Some(Value::Array(parts)) = item.get("content") {
        for part in parts {
            let text = content_part_to_text(part);
            if !text.is_empty() {
                messages.push(json!({"role": role, "content": text}));
            }
        }
        return;
    }
    let text = input_item_text(item);
    if !text.is_empty() {
        messages.push(json!({"role": role, "content": text}));
    }
}

fn function_call_output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match t {
                    "input_text" | "text" | "output_text" => part
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    _ => value_to_text(part),
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(v) => value_to_text(v).unwrap_or_default(),
        None => String::new(),
    }
}

fn content_part_to_text(part: &Value) -> String {
    let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "input_text" | "text" | "output_text" => part
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        _ => value_to_text(part).unwrap_or_default(),
    }
}

fn is_role_name(v: &Value) -> bool {
    matches!(
        v.as_str(),
        Some("user") | Some("assistant") | Some("system") | Some("developer")
    )
}

fn normalize_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        other => other,
    }
}

fn input_item_text(item: &Value) -> String {
    if let Some(s) = item.as_str() {
        return s.to_string();
    }
    if let Some(content) = item.get("content") {
        return input_content_to_text(content);
    }
    value_to_text(item).unwrap_or_default()
}

fn input_content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match t {
                    "input_text" | "text" | "output_text" => part
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    _ => value_to_text(part),
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => value_to_text(content).unwrap_or_default(),
    }
}

fn value_to_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(value_to_text)
                .collect::<Vec<_>>()
                .join("\n\n");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_like_request_to_completions() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/requests/responses/codex_basic.json"
        ));
        let out = responses_request_to_completions(&body, "deepseek-chat", None).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "deepseek-chat");
        assert_eq!(v["stream"], true);
        let msgs = v["messages"].as_array().unwrap();
        assert!(msgs.len() >= 2);
        assert_eq!(msgs[0]["role"], "user");
        assert!(msgs[0]["content"].as_str().unwrap().contains("permissions"));
        assert!(msgs.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("hi"));
    }

    #[test]
    fn completions_response_to_responses_shape() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/response/completions/basic.json"
        ));
        let out = completions_response_to_responses(&body, "gpt-5.5").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["object"], "response");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["model"], "gpt-5.5");
        assert_eq!(v["output"][0]["type"], "message");
        assert!(v["output"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Sorry"));
    }

    #[test]
    fn instructions_become_system_message() {
        let body = Bytes::from_static(
            br#"{"model":"m","instructions":"be helpful","input":"hello","stream":false}"#,
        );
        let out = responses_request_to_completions(&body, "upstream", None).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "be helpful");
        assert_eq!(v["messages"][1]["content"], "hello");
    }

    #[test]
    fn tool_roundtrip_request_to_completions() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/requests/responses/codex_tool_roundtrip.json"
        ));
        let out = responses_request_to_completions(&body, "deepseek-chat", None).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert!(v.get("tools").is_some());
        let tool = &v["tools"][0];
        assert!(tool.get("function").is_some());
        assert_eq!(tool["function"]["name"], "shell");
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"][0]["function"]["name"], "shell");
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_abc123");
        assert!(msgs[2]["content"].as_str().unwrap().contains("Cargo.toml"));
    }

    #[test]
    fn completions_tool_call_to_responses_output() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/response/completions/tool_call.json"
        ));
        let out = completions_response_to_responses(&body, "gpt-5.5").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["output"][0]["type"], "function_call");
        assert_eq!(v["output"][0]["call_id"], "call_abc123");
        assert_eq!(v["output"][0]["name"], "shell");
        assert!(v["output"][0]["arguments"]
            .as_str()
            .unwrap()
            .contains("ls -la"));
    }

    #[test]
    fn batches_consecutive_function_calls_into_one_assistant_message() {
        let body = json!({
            "model": "gpt-5.5",
            "stream": true,
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"go"}]},
                {"type":"function_call","call_id":"call_a","name":"shell","arguments":"{}"},
                {"type":"function_call","call_id":"call_b","name":"read","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_a","output":"ok"},
                {"type":"function_call_output","call_id":"call_b","output":"file"}
            ]
        });
        let out = responses_request_to_completions(
            &Bytes::from(serde_json::to_vec(&body).unwrap()),
            "deepseek-chat",
            None,
        )
        .unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["tool_call_id"], "call_a");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_b");
    }
}
