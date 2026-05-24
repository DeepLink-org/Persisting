//! Anthropic Messages ↔ OpenAI Chat Completions (minimal text-only subset).

use anyhow::{Context, Result};
use bytes::Bytes;
use serde_json::{json, Value};

/// Convert Anthropic `/v1/messages` JSON body to OpenAI `/v1/chat/completions`.
pub fn messages_request_to_completions(body: &Bytes, upstream_model: &str) -> Result<Bytes> {
    let mut v: Value = serde_json::from_slice(body).context("parse messages request")?;
    let obj = v
        .as_object_mut()
        .context("messages request must be a JSON object")?;

    obj.insert("model".to_string(), json!(upstream_model));

    let mut out_messages = Vec::new();
    if let Some(system) = obj.remove("system") {
        if let Some(text) = system_to_text(&system) {
            out_messages.push(json!({"role": "system", "content": text}));
        }
    }

    if let Some(msgs) = obj.get("messages").and_then(|m| m.as_array()).cloned() {
        for msg in msgs {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let content = anthropic_content_to_openai(msg.get("content").unwrap_or(&Value::Null));
            out_messages.push(json!({"role": role, "content": content}));
        }
    }
    obj.insert("messages".to_string(), json!(out_messages));

    for key in [
        "top_k",
        "metadata",
        "thinking",
        "output_config",
        "tool_choice",
        "tools",
    ] {
        obj.remove(key);
    }

    Ok(Bytes::from(
        serde_json::to_vec(&v).context("serialize completions request")?,
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
    let text = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let finish = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("stop");
    let id = body
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("msg_proxy");
    let usage = openai_usage_to_anthropic(body.get("usage"));

    Ok(json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": client_model,
        "content": [{"type": "text", "text": text}],
        "stop_reason": openai_finish_to_anthropic(finish),
        "stop_sequence": null,
        "usage": usage,
    }))
}

fn system_to_text(system: &Value) -> Option<String> {
    match system {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|b| {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .filter(|t| !t.trim().is_empty())
                })
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

fn anthropic_content_to_openai(content: &Value) -> Value {
    match content {
        Value::String(s) => json!(s),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            parts.push(json!({"type": "text", "text": text}));
                        }
                    }
                    Some("tool_result") => {
                        let tool_content =
                            block.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        parts.push(json!({
                            "type": "text",
                            "text": format!("[tool_result:{}] {}", block.get("tool_use_id").and_then(|v| v.as_str()).unwrap_or(""), tool_content)
                        }));
                    }
                    _ => {}
                }
            }
            if parts.len() == 1 {
                parts.into_iter().next().unwrap_or(json!(""))
            } else if parts.is_empty() {
                json!("")
            } else {
                json!(parts)
            }
        }
        _ => json!(""),
    }
}

fn openai_finish_to_anthropic(finish: &str) -> &'static str {
    match finish {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn openai_usage_to_anthropic(usage: Option<&Value>) -> Value {
    let Some(u) = usage else {
        return json!({
            "input_tokens": 0,
            "output_tokens": 0,
        });
    };
    json!({
        "input_tokens": u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        "output_tokens": u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0),
        "cache_read_input_tokens": u
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
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
        assert_eq!(v["messages"][0]["content"], "Hello, world");
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
        assert_eq!(v["messages"][1]["content"], "hi");
    }
}
