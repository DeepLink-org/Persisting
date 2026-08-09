use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RunSummary {
    pub agent_id: String,
    pub session_id: String,
    pub root_session_id: Option<String>,
    pub row_count: usize,
    pub duplicate_event_ids: usize,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryCatalog {
    pub database: String,
    pub storage_path: String,
    pub path_column: String,
    pub tables: Vec<QueryTableSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryTableSummary {
    pub name: String,
    pub description: String,
    pub grain: String,
    pub fields: Vec<QueryFieldSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryFieldSummary {
    pub name: String,
    pub data_type: String,
    pub description: String,
}

impl RunSummary {
    pub fn query(&self) -> String {
        let mut out = format!(
            "agent_id={}&session_id={}",
            urlencoding::encode(&self.agent_id),
            urlencoding::encode(&self.session_id)
        );
        if let Some(root) = &self.root_session_id {
            out.push_str("&root_session_id=");
            out.push_str(&urlencoding::encode(root));
        }
        out
    }

    pub fn search_text(&self) -> String {
        format!(
            "{} {} {}",
            self.agent_id,
            self.session_id,
            self.root_session_id.as_deref().unwrap_or("")
        )
        .to_ascii_lowercase()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct StorylineTurn {
    pub id: i64,
    pub kind: Option<String>,
    #[serde(rename = "ts", alias = "timestamp")]
    pub timestamp: Option<String>,
    #[serde(rename = "src", alias = "source")]
    pub source: String,
    #[serde(rename = "msg", alias = "message")]
    pub message: Value,
    #[serde(rename = "reason", alias = "reasoning_content")]
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub observation: Option<Value>,
    pub metrics: Option<Value>,
    #[serde(rename = "model", alias = "model_name")]
    pub model_name: Option<String>,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub extra: Option<Value>,
}

impl StorylineTurn {
    pub fn text(&self) -> String {
        match &self.message {
            Value::String(value) => value.clone(),
            value => serde_json::to_string_pretty(value).unwrap_or_default(),
        }
    }

    pub fn searchable_text(&self) -> String {
        format!(
            "{} {} {}",
            self.source,
            self.kind.as_deref().unwrap_or(""),
            self.text()
        )
        .to_ascii_lowercase()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolCall {
    #[serde(rename = "tcid", alias = "tool_call_id")]
    pub tool_call_id: String,
    #[serde(rename = "fn", alias = "function_name")]
    pub function_name: String,
    #[serde(rename = "args", alias = "arguments")]
    pub arguments: Value,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnView {
    pub turn: StorylineTurn,
    pub call_id: Option<String>,
    pub event_seqs: Vec<u64>,
    #[serde(default)]
    pub wire_tool_calls: Vec<WireToolCall>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct WireToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TrajectoryView {
    pub run: RunSummary,
    pub event_kind_counts: BTreeMap<String, usize>,
    pub tool_call_count: usize,
    pub turns: Vec<TurnView>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EventRecord {
    pub seq: u64,
    pub source: String,
    pub kind: String,
    pub timestamp: Option<String>,
    pub event_id: Option<String>,
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub producer: Option<String>,
    pub payload: Value,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct EventsPage {
    pub snapshot: EventsSnapshot,
    pub records: Vec<EventRecord>,
}

#[derive(Debug, Deserialize)]
pub struct EventsSnapshot {
    pub next_offset: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct StreamSnapshot {
    pub row_count: Option<usize>,
    pub status: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_query_encodes_coordinates() {
        let run = RunSummary {
            agent_id: "agent one".into(),
            session_id: "s/1".into(),
            root_session_id: Some("root+1".into()),
            row_count: 2,
            duplicate_event_ids: 0,
            status: "ok".into(),
        };
        assert_eq!(
            run.query(),
            "agent_id=agent%20one&session_id=s%2F1&root_session_id=root%2B1"
        );
    }
}
