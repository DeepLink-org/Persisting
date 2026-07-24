//! `CaptureRecord` and Lance append wire encoding (RON lines between engine components).

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use crate::protocol::ProtocolKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub seq: u64,
    pub source: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub payload: serde_json::Value,
}

impl CaptureRecord {
    /// Internal traffic (e.g. `count_tokens`) — not a dialogue turn.
    pub fn is_internal_llm_request(&self) -> bool {
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

    /// Visible user text — canonical for turn indexing, markdown blocks, and filters.
    ///
    /// Prefers summary field `user_content`, then walks nested proxy `body` / messages.
    pub fn visible_user_text(&self) -> Option<String> {
        visible_user_from_payload(&self.payload)
    }

    /// Visible assistant text — canonical for turn indexing, markdown blocks, and filters.
    ///
    /// Prefers `assistant_content`, then SSE / JSON body parsing (including proxy wrappers).
    pub fn visible_assistant_text(&self) -> Option<String> {
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

/// One engine append line (RON) from a JSON value.
pub fn json_to_engine_line(v: &serde_json::Value) -> anyhow::Result<String> {
    ron::to_string(v).context("encode engine line")
}

pub fn json_str_to_engine_line(json: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).context("decode JSON")?;
    json_to_engine_line(&v)
}

pub fn engine_line_to_json(line: &str) -> anyhow::Result<String> {
    let v: serde_json::Value = ron::from_str(line.trim()).context("decode engine line")?;
    serde_json::to_string(&v).context("encode JSON")
}

pub fn record_to_engine_line(rec: &CaptureRecord) -> anyhow::Result<String> {
    json_to_engine_line(&serde_json::to_value(rec).context("record to JSON")?)
}

pub fn engine_line_to_record(line: &str) -> anyhow::Result<CaptureRecord> {
    let json = engine_line_to_json(line)?;
    serde_json::from_str(&json).context("decode CaptureRecord")
}

pub fn records_to_engine_lines(records: &[CaptureRecord]) -> anyhow::Result<String> {
    records
        .iter()
        .enumerate()
        .map(|(i, rec)| record_to_engine_line(rec).with_context(|| format!("record[{i}]")))
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
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
        let rec = CaptureRecord {
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
