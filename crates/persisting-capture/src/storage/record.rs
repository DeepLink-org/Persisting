//! `CaptureRecord` and Lance append wire encoding (RON lines between engine components).

use anyhow::Context;
use serde::{Deserialize, Serialize};

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

    /// Visible user text for turn indexing and markdown (with body fallback).
    pub fn visible_user_text(&self) -> Option<String> {
        self.payload
            .get("user_content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| user_message_from_payload(&self.payload))
            .filter(|s| !s.trim().is_empty())
    }

    /// Visible assistant text for turn indexing and markdown (with body fallback).
    pub fn visible_assistant_text(&self) -> Option<String> {
        self.payload
            .get("assistant_content")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| assistant_message_from_payload(&self.payload))
            .filter(|s| !s.trim().is_empty())
    }
}

fn user_message_from_payload(payload: &serde_json::Value) -> Option<String> {
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

fn assistant_message_from_payload(payload: &serde_json::Value) -> Option<String> {
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
    use crate::sink::llm_request_summary_record;
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
}
