//! `CaptureRecord` and Lance append wire encoding (RON lines between engine components).

use anyhow::Context;
use serde::{Deserialize, Serialize};

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
