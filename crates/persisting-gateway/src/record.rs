//! Capture-side behavior over the shared
//! [`EventRecord`](persisting_events::EventRecord) schema.

use serde_json::Value;

use super::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::protocol::ProtocolKind;

pub use persisting_events::EventRecord;

/// Capture-only interpretation of raw proxy payloads.
///
/// The record schema belongs to the shared events contract; SSE and provider
/// payload extraction remain producer concerns and are extension behavior.
pub trait EventRecordExt {
    /// Internal traffic (e.g. `count_tokens`) — not a dialogue turn.
    fn is_internal_llm_request(&self) -> bool;

    /// Visible user text used by Capture's live dialogue projection.
    fn visible_user_text(&self) -> Option<String>;

    /// Visible assistant text used by Capture's live dialogue projection.
    fn visible_assistant_text(&self) -> Option<String>;
}

impl EventRecordExt for EventRecord {
    fn is_internal_llm_request(&self) -> bool {
        if self.kind != "llm.request" {
            return false;
        }
        if self
            .payload
            .get("protocol")
            .and_then(|p| p.as_str())
            .is_some_and(|p| p == ProtocolKind::CountTokens.as_str())
        {
            return true;
        }
        self.payload
            .get("path")
            .and_then(|p| p.as_str())
            .is_some_and(|path| ProtocolKind::from_path(path) == ProtocolKind::CountTokens)
    }

    fn visible_user_text(&self) -> Option<String> {
        visible_user_from_payload(&self.payload)
    }

    fn visible_assistant_text(&self) -> Option<String> {
        visible_assistant_from_payload(&self.payload)
    }
}

/// Parse structured message `content` (string or Anthropic-style blocks).
pub(crate) fn content_to_string(v: &Value) -> Option<String> {
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

fn visible_user_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("user_content").and_then(|v| v.as_str()) {
        return non_empty(s);
    }
    let messages = llm_inner_body(payload)
        .and_then(|b| b.get("messages"))
        .or_else(|| payload.get("messages"))?
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

fn visible_assistant_from_payload(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("assistant_content").and_then(|v| v.as_str()) {
        return non_empty(s);
    }
    if let Some(s) = payload.get("body").and_then(|b| b.as_str()) {
        let text = extract_assistant_turn_from_sse(s);
        if let Some(t) = non_empty(&text) {
            return Some(t);
        }
    }
    if let Some(inner) = llm_inner_body(payload) {
        if let Some(s) = inner.as_str() {
            let text = extract_assistant_turn_from_sse(s);
            if let Some(t) = non_empty(&text) {
                return Some(t);
            }
        }
        if let Some(text) = extract_assistant_text_from_json(inner) {
            return non_empty(&text);
        }
    }
    llm_inner_body(payload)
        .and_then(|b| b.get("choices"))
        .or_else(|| payload.get("body").and_then(|b| b.get("choices")))
        .or_else(|| payload.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(content_to_string)
        .or_else(|| payload.get("content").and_then(content_to_string))
        .and_then(|s| non_empty(&s))
}

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Inner LLM JSON: `payload.body` or proxy wrapper `payload.body.body`.
fn llm_inner_body(payload: &Value) -> Option<&Value> {
    let wrap = payload.get("body")?;
    if wrap.get("messages").is_some() || wrap.get("choices").is_some() {
        Some(wrap)
    } else {
        wrap.get("body")
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::sink::{llm_request_record, llm_request_summary_record, llm_response_record};
    use crate::Call;

    #[test]
    fn internal_request_detects_count_tokens_path() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let rec = llm_request_summary_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/messages/count_tokens",
            10,
            "count_tokens",
            "openai",
            None,
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        assert!(rec.is_internal_llm_request());
    }

    #[test]
    fn visible_user_prefers_user_content_field() {
        let rec = EventRecord {
            identity: Default::default(),
            seq: 0,
            source: "test".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "user_content": "hello",
                "body": {"messages": [{"role": "user", "content": "ignored"}]}
            }),
        };
        assert_eq!(rec.visible_user_text().as_deref(), Some("hello"));
    }

    #[test]
    fn visible_user_reads_proxy_nested_body() {
        let req = llm_request_record(
            Some("sess".into()),
            Some("agent".into()),
            "mock-model",
            "/v1/chat/completions",
            &serde_json::json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "body": {"messages":[{"role":"user","content":"你好"}],"model":"mock-model"},
            }),
        );
        assert_eq!(req.visible_user_text().as_deref(), Some("你好"));
    }

    #[test]
    fn visible_assistant_reads_proxy_nested_body() {
        let resp = llm_response_record(
            Some("sess".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "body": {
                    "choices":[{"message":{"role":"assistant","content":"你好！"}}],
                },
            }),
            false,
            &Call {
                call_id: "c".into(),
                trace_id: "t".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
        );
        assert_eq!(resp.visible_assistant_text().as_deref(), Some("你好！"));
    }
}
