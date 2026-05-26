use serde_json::Value;

use crate::dialogue_extract::is_subagent_shape_payload;
use crate::record::CaptureRecord;

pub fn should_skip_record(rec: &CaptureRecord) -> bool {
    match rec.kind.as_str() {
        "llm.request" => {
            if is_internal_llm_request(&rec.payload) {
                return true;
            }
            if should_skip_main_flash_companion_request(rec) {
                return true;
            }
            visible_user_text(rec).is_none()
        }
        "llm.response" | "llm.response.stream" => {
            if rec.payload.get("stream_partial").and_then(|v| v.as_bool()) == Some(true) {
                return true;
            }
            visible_assistant_text(rec).is_none()
        }
        "llm.spawn_link" => false,
        "llm.call.cancelled" => true,
        k if k.starts_with("session.") => true,
        _ => false,
    }
}

pub fn should_refresh_frontmatter(rec: &CaptureRecord) -> bool {
    matches!(
        rec.kind.as_str(),
        "llm.request" | "llm.response" | "llm.response.stream"
    )
}

fn is_internal_llm_request(payload: &Value) -> bool {
    payload
        .get("path")
        .and_then(|p| p.as_str())
        .is_some_and(|p| p.contains("count_tokens") || p.contains("count-tokens"))
}

fn visible_user_text(rec: &CaptureRecord) -> Option<String> {
    rec.payload
        .get("user_content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| user_message_from_payload(&rec.payload))
        .filter(|s| !s.trim().is_empty())
}

fn visible_assistant_text(rec: &CaptureRecord) -> Option<String> {
    rec.payload
        .get("assistant_content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| assistant_message_from_payload(&rec.payload))
        .filter(|s| !s.trim().is_empty())
}

fn user_message_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("user_content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    payload
        .get("body")
        .and_then(|b| b.get("messages"))
        .and_then(|m| m.as_array())
        .and_then(|msgs| {
            msgs.iter().rev().find_map(|msg| {
                if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                    return None;
                }
                msg.get("content")
                    .and_then(|c| c.as_str())
                    .map(str::to_string)
            })
        })
}

fn assistant_message_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("assistant_content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    payload
        .get("body")
        .and_then(|b| b.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|choices| choices.first())
        .and_then(|ch| ch.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::to_string)
}

fn should_skip_main_flash_companion_request(rec: &CaptureRecord) -> bool {
    if rec.subagent_id.is_some() {
        return false;
    }
    let model = rec
        .payload
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if !model.contains("flash") && !model.contains("haiku") {
        return false;
    }
    if visible_user_text(rec).is_none() {
        return false;
    }
    !is_subagent_shape_payload(&rec.payload)
}
