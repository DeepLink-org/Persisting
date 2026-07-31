//! Claude Code request-shape heuristics (subagent probes, task notifications).

use bytes::Bytes;
use serde_json::Value;

/// Main agent turn delivered with a subagent's `x-claude-code-agent-id` header
/// (e.g. `<task-notification>` after background agent completion).
pub fn is_main_agent_continuation_payload(payload: &Value) -> bool {
    if payload
        .get("user_content")
        .and_then(|s| s.as_str())
        .is_some_and(contains_task_notification)
    {
        return true;
    }
    payload
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|msgs| msgs.iter().any(message_contains_task_notification))
}

pub fn is_main_agent_continuation(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .is_some_and(|v| is_main_agent_continuation_payload(&v))
}

fn message_contains_task_notification(msg: &Value) -> bool {
    match msg.get("content") {
        Some(Value::String(s)) => contains_task_notification(s),
        Some(Value::Array(parts)) => parts.iter().any(|p| {
            p.get("text")
                .and_then(|t| t.as_str())
                .is_some_and(contains_task_notification)
        }),
        _ => false,
    }
}

fn contains_task_notification(s: &str) -> bool {
    s.contains("<task-notification>")
}

/// Claude Code subagent probe (`*-flash`/`*-haiku` + `<session>` wrapper).
pub fn is_subagent_shape_payload(payload: &Value) -> bool {
    let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("");
    if !model.contains("flash") && !model.contains("haiku") {
        return false;
    }
    if payload
        .get("user_content")
        .and_then(|s| s.as_str())
        .is_some_and(|s| s.contains("<session>"))
    {
        return true;
    }
    payload
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|msgs| {
            msgs.iter().any(|msg| {
                msg.get("content")
                    .map(content_contains_session_tag)
                    .unwrap_or(false)
            })
        })
}

fn content_contains_session_tag(c: &Value) -> bool {
    match c {
        Value::String(s) => s.contains("<session>"),
        Value::Array(parts) => parts.iter().any(|p| {
            p.get("text")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.contains("<session>"))
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn detects_task_notification_as_main_continuation() {
        let body = br#"{"model":"deepseek-v4-pro","messages":[{"role":"user","content":[{"type":"text","text":"<task-notification>\n<task-id>abc12345</task-id>\n</task-notification>"}]}]}"#;
        assert!(is_main_agent_continuation(&Bytes::from_static(body)));
    }

    #[test]
    fn subagent_flash_prompt_is_not_main_continuation() {
        let body = br#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"Review docs"}]}"#;
        assert!(!is_main_agent_continuation(&Bytes::from_static(body)));
    }
}
