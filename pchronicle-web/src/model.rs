use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct RunSummary {
    #[serde(default = "default_dataset_name")]
    pub dataset: String,
    #[serde(default = "default_source_file")]
    pub file: String,
    #[serde(default)]
    pub run_id: Option<String>,
    pub agent_id: String,
    #[serde(default)]
    pub model_name: Option<String>,
    pub session_id: String,
    pub root_session_id: Option<String>,
    pub path: String,
    pub row_count: usize,
    pub duplicate_event_ids: usize,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunExplorerItem {
    #[serde(flatten)]
    pub run: RunSummary,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PageSnapshot {
    pub offset: usize,
    pub next_offset: usize,
    pub total: usize,
    pub has_more: bool,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunPage {
    pub snapshot: PageSnapshot,
    pub records: Vec<RunExplorerItem>,
    pub path_index: Vec<RunSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryCatalog {
    #[serde(default)]
    pub snapshot_id: String,
    #[serde(default)]
    pub read_only: bool,
    pub database: String,
    pub storage_path: String,
    pub path_column: String,
    #[serde(default)]
    pub datasets: Vec<QueryDatasetSummary>,
    pub tables: Vec<QueryTableSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryDatasetSummary {
    pub name: String,
    pub uri: String,
    pub ready_sources: usize,
    pub error_sources: usize,
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
            "dataset={}&file={}",
            urlencoding::encode(&self.dataset),
            urlencoding::encode(&self.file)
        );
        if let Some(run_id) = self.run_id.as_deref().filter(|value| !value.is_empty()) {
            out.push_str("&run_id=");
            out.push_str(&urlencoding::encode(run_id));
        }
        out.push_str(&format!(
            "&agent_id={}&session_id={}",
            urlencoding::encode(&self.agent_id),
            urlencoding::encode(&self.session_id)
        ));
        if let Some(root) = &self.root_session_id {
            out.push_str("&root_session_id=");
            out.push_str(&urlencoding::encode(root));
        }
        out
    }
}

fn default_dataset_name() -> String {
    "dataset".into()
}

fn default_source_file() -> String {
    ".".into()
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ToolCall {
    #[serde(rename = "tcid", alias = "tool_call_id")]
    pub tool_call_id: String,
    #[serde(rename = "fn", alias = "function_name")]
    pub function_name: String,
    #[serde(rename = "args", alias = "arguments")]
    pub arguments: Value,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct WireToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct MetricStats {
    pub sample_count: usize,
    pub total_count: usize,
    pub p50: Option<f64>,
    pub p95: Option<f64>,
    pub max: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolAggregate {
    pub name: String,
    pub count: usize,
    pub duration_sample_count: usize,
    pub total_duration_ms: Option<f64>,
    pub average_duration_ms: Option<f64>,
    pub max_duration_ms: Option<f64>,
    pub error_associated_count: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct DimensionAggregate {
    pub name: String,
    pub turn_count: usize,
    pub error_count: usize,
    pub latency_sample_count: usize,
    pub average_latency_ms: Option<f64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct HistogramBucket {
    pub label: String,
    pub lower_bound_ms: f64,
    pub upper_bound_ms: Option<f64>,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunAnalysis {
    pub run: RunSummary,
    pub event_count: usize,
    pub turn_count: usize,
    pub tool_call_count: usize,
    pub error_count: usize,
    pub start_timestamp: Option<String>,
    pub end_timestamp: Option<String>,
    pub models: Vec<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub latency_ms: MetricStats,
    pub ttft_ms: MetricStats,
    pub latency_histogram: Vec<HistogramBucket>,
    pub source_breakdown: Vec<DimensionAggregate>,
    pub kind_breakdown: Vec<DimensionAggregate>,
    pub model_breakdown: Vec<DimensionAggregate>,
    pub tools: Vec<ToolAggregate>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct TurnSummary {
    pub id: i64,
    pub source: String,
    pub kind: Option<String>,
    pub timestamp: Option<String>,
    pub call_id: Option<String>,
    pub preview: String,
    pub model_name: Option<String>,
    pub latency_ms: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub tool_names: Vec<String>,
    pub event_seqs: Vec<u64>,
    pub has_error: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnPage {
    pub snapshot: PageSnapshot,
    pub records: Vec<TurnSummary>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnDetail {
    pub summary: TurnSummary,
    pub turn: StorylineTurn,
    pub wire_tool_calls: Vec<WireToolCall>,
    pub events: Vec<EventRecord>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct QueryEvidence {
    pub rows: Vec<Value>,
    pub returned_rows: usize,
    pub truncated: bool,
    pub max_rows: usize,
    pub max_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_query_encodes_coordinates() {
        let run = RunSummary {
            dataset: "dataset".into(),
            file: "nested/source.json".into(),
            run_id: Some("run-1".into()),
            agent_id: "agent one".into(),
            model_name: None,
            session_id: "s/1".into(),
            root_session_id: Some("root+1".into()),
            path: "agent one/root+1/subagents/s-1".into(),
            row_count: 2,
            duplicate_event_ids: 0,
            status: "ok".into(),
        };
        assert_eq!(
            run.query(),
            "dataset=dataset&file=nested%2Fsource.json&run_id=run-1&agent_id=agent%20one&session_id=s%2F1&root_session_id=root%2B1"
        );
    }

    #[test]
    fn run_page_accepts_nullable_run_id() {
        let page: RunPage = serde_json::from_value(serde_json::json!({
            "snapshot": {
                "offset": 0,
                "next_offset": 1,
                "total": 1,
                "has_more": false,
                "limit": 100
            },
            "records": [{
                "dataset": "captures",
                "file": "capture-comparison/events.lance",
                "document_id": "session-1",
                "run_id": null,
                "agent_id": "capture-comparison",
                "model_name": null,
                "session_id": "session-1",
                "root_session_id": null,
                "path": "captures/capture-comparison/session-1",
                "row_count": 6,
                "duplicate_event_ids": 0,
                "status": "completed",
                "model": "test-model"
            }],
            "path_index": []
        }))
        .expect("canonical Gateway runs may not have a run id");

        assert_eq!(page.records[0].run.run_id, None);
        assert_eq!(
            page.records[0].run.query(),
            "dataset=captures&file=capture-comparison%2Fevents.lance&agent_id=capture-comparison&session_id=session-1"
        );
    }
}
