//! JSON-oriented visible text extraction for [`EventRecord`] mapping.
//!
//! Live SSE extraction stays in capture; stamp `user_content` / `assistant_content`
//! when those paths matter.

use serde_json::Value;

use crate::formats::events::EventRecord;

pub(super) fn content_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let out: Vec<_> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if out.is_empty() {
                None
            } else {
                Some(out.join("\n"))
            }
        }
        _ => None,
    }
}

pub(super) fn compact_json(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
}

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn llm_inner_body(payload: &Value) -> Option<&Value> {
    payload
        .get("body")
        .filter(|b| b.is_object() || b.is_array() || b.is_string())
}

pub(super) fn visible_user_text(rec: &EventRecord) -> Option<String> {
    if let Some(s) = rec.payload.get("user_content").and_then(|v| v.as_str()) {
        return non_empty(s);
    }
    let messages = llm_inner_body(&rec.payload)
        .and_then(|b| b.get("messages"))
        .or_else(|| rec.payload.get("messages"))?
        .as_array()?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let Some(text) = msg.get("content").and_then(content_to_string) {
                return non_empty(&text);
            }
        }
    }
    None
}

pub(super) fn visible_assistant_text(rec: &EventRecord) -> Option<String> {
    if let Some(s) = rec
        .payload
        .get("assistant_content")
        .and_then(|v| v.as_str())
    {
        return non_empty(s);
    }
    llm_inner_body(&rec.payload)
        .and_then(|b| b.get("choices"))
        .or_else(|| rec.payload.get("body").and_then(|b| b.get("choices")))
        .or_else(|| rec.payload.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(content_to_string)
        .or_else(|| {
            llm_inner_body(&rec.payload)
                .and_then(|b| b.get("content"))
                .and_then(content_to_string)
        })
        .or_else(|| rec.payload.get("content").and_then(content_to_string))
        .and_then(|s| non_empty(&s))
}
