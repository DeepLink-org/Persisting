use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

fn deserialize_optional_timestamp<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum TimestampValue {
        Text(String),
        Integer(i64),
        Float(f64),
    }

    Ok(match Option::<TimestampValue>::deserialize(deserializer)? {
        None => None,
        Some(TimestampValue::Text(value)) if value.is_empty() => None,
        Some(TimestampValue::Text(value)) => Some(value),
        Some(TimestampValue::Integer(value)) => Some(value.to_string()),
        Some(TimestampValue::Float(value)) => Some(value.to_string()),
    })
}

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
    #[serde(default)]
    pub search_preview: Option<String>,
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
    #[serde(default)]
    pub search: RunSearchStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct RunSearchStatus {
    #[serde(default)]
    pub fts_available: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub tokenizer: Option<String>,
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
pub struct CatalogTree {
    #[serde(default)]
    pub dataset: Option<String>,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub run_count: usize,
    #[serde(default)]
    pub failed_count: usize,
    #[serde(default)]
    pub ready_sources: Option<usize>,
    #[serde(default)]
    pub error_sources: Option<usize>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub children: Vec<CatalogTreeChild>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CatalogTreeChild {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub run_count: usize,
    #[serde(default)]
    pub failed_count: usize,
    #[serde(default)]
    pub entries: Vec<CatalogTreeChild>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryTableSummary {
    pub name: String,
    pub description: String,
    #[serde(default = "default_table_kind")]
    pub kind: String,
    pub grain: String,
    pub fields: Vec<QueryFieldSummary>,
}

fn default_table_kind() -> String {
    "table".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueryFieldSummary {
    pub name: String,
    pub data_type: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalSource {
    pub dataset: String,
    pub file: String,
    pub format: String,
    pub uri: String,
    pub size_bytes: Option<u64>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalLayout {
    pub dataset: String,
    pub file: String,
    pub format: String,
    pub tables: Vec<PhysicalTable>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalTable {
    pub name: String,
    pub uri: String,
    pub version: u64,
    pub num_rows: u64,
    pub fragments: Vec<PhysicalFragment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalFragment {
    pub id: u64,
    pub physical_rows: Option<u64>,
    pub size_bytes: Option<u64>,
    pub deletion_file: Option<String>,
    pub files: Vec<PhysicalDataFile>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalDataFile {
    pub path: String,
    pub field_ids: Vec<i32>,
    pub field_names: Vec<String>,
    pub size_bytes: Option<u64>,
    pub encoding: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalFileLayout {
    pub table: String,
    pub fragment_id: u64,
    pub data_file: String,
    pub num_rows: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub remaining_columns: usize,
    pub columns: Vec<PhysicalColumn>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalColumn {
    pub name: String,
    pub field_id: i32,
    #[serde(default)]
    pub data_type: String,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default)]
    pub null_count: u64,
    #[serde(default)]
    pub non_null_count: u64,
    #[serde(default)]
    pub compressed_bytes: Option<u64>,
    #[serde(default)]
    pub uncompressed_bytes: Option<u64>,
    #[serde(default)]
    pub max_value: Option<PhysicalExtremeValue>,
    #[serde(default)]
    pub value_distribution: Vec<PhysicalBucket>,
    #[serde(default)]
    pub size_distribution: Vec<PhysicalBucket>,
    pub pages: Vec<PhysicalPage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalExtremeValue {
    pub row_offset: u64,
    pub size_bytes: u64,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalBucket {
    pub label: String,
    pub count: u64,
    pub weight: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PhysicalPage {
    pub index: u32,
    pub offset: u64,
    pub size: u64,
    pub num_rows: Option<u64>,
    pub encoding: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PhysicalPagePreview {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub offset: usize,
    pub limit: usize,
    pub truncated: bool,
    pub truncated_cells: usize,
}

pub fn queryable_tables(catalog: &QueryCatalog) -> Vec<QueryTableSummary> {
    if catalog.tables.iter().any(|table| table.name.contains('.')) {
        return catalog.tables.clone();
    }
    let datasets: Vec<String> = if catalog.datasets.is_empty() {
        if catalog.database.trim().is_empty() {
            Vec::new()
        } else {
            vec![catalog.database.clone()]
        }
    } else {
        catalog
            .datasets
            .iter()
            .map(|dataset| dataset.name.clone())
            .collect()
    };
    datasets
        .into_iter()
        .flat_map(|dataset| {
            catalog.tables.iter().map(move |table| {
                let mut qualified = table.clone();
                qualified.name = format!("{dataset}.{}", table.name);
                qualified
            })
        })
        .collect()
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
    #[serde(
        rename = "ts",
        alias = "timestamp",
        default,
        deserialize_with = "deserialize_optional_timestamp"
    )]
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
        extract_message_text(&self.message)
            .unwrap_or_else(|| serde_json::to_string_pretty(&self.message).unwrap_or_default())
    }
}

/// Extract human-readable text from common message shapes:
/// - plain string
/// - `{ "type": "text", "text": "..." }` object
/// - `[{ "type": "text", "text": "..." }, ...]` content array
pub fn extract_message_text(message: &Value) -> Option<String> {
    match message {
        Value::String(value) => Some(value.clone()),
        Value::Object(object) => object
            .get("text")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        Value::Array(array) => {
            let mut parts = Vec::new();
            for item in array {
                if let Some(text) = extract_message_text(item) {
                    if !text.is_empty() {
                        parts.push(text);
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
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
    #[serde(default)]
    pub result: Option<Value>,
    pub duration_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct WireToolCall {
    pub id: Option<String>,
    pub name: String,
    pub arguments: Value,
    #[serde(default)]
    pub result: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EventRecord {
    pub seq: u64,
    pub source: String,
    pub kind: String,
    #[serde(default, deserialize_with = "deserialize_optional_timestamp")]
    pub timestamp: Option<String>,
    pub event_id: Option<String>,
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub producer: Option<String>,
    pub payload: Value,
    #[serde(flatten)]
    pub rest: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventProvenance {
    Canonical,
    SyntheticFromStoryline,
}

impl EventProvenance {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::SyntheticFromStoryline => "synthetic_from_storyline",
        }
    }

    /// Plain-language label for user-facing summaries.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Canonical => crate::terminology::RECORDED_EVENTS,
            Self::SyntheticFromStoryline => crate::terminology::RECONSTRUCTED_EVENTS,
        }
    }
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
    pub event_provenance: EventProvenance,
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
    #[serde(default, deserialize_with = "deserialize_optional_timestamp")]
    pub timestamp: Option<String>,
    pub call_id: Option<String>,
    pub preview: String,
    #[serde(default)]
    pub user_prompt: Option<String>,
    #[serde(default)]
    pub char_count: u64,
    #[serde(default)]
    pub modalities: Vec<String>,
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
    #[serde(default)]
    pub search: TurnSearchStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct TurnSearchStatus {
    #[serde(default)]
    pub fts_available: bool,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub tokenizer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct TurnDetail {
    pub summary: TurnSummary,
    pub turn: StorylineTurn,
    pub wire_tool_calls: Vec<WireToolCall>,
    pub event_provenance: EventProvenance,
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

    #[test]
    fn turn_detail_accepts_numeric_storyline_timestamps() {
        let turn: StorylineTurn = serde_json::from_value(serde_json::json!({
            "id": 85,
            "src": "user",
            "msg": "hello",
            "ts": 1785310111
        }))
        .expect("explorer detail preserves numeric Storyline timestamps");
        assert_eq!(turn.timestamp.as_deref(), Some("1785310111"));

        let event: EventRecord = serde_json::from_value(serde_json::json!({
            "seq": 1,
            "source": "gateway",
            "kind": "llm.request",
            "timestamp": 1785310111,
            "payload": {}
        }))
        .expect("linked events may carry numeric timestamps");
        assert_eq!(event.timestamp.as_deref(), Some("1785310111"));

        let rfc3339: StorylineTurn = serde_json::from_value(serde_json::json!({
            "id": 1,
            "src": "agent",
            "msg": "ok",
            "ts": "2026-07-29T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(rfc3339.timestamp.as_deref(), Some("2026-07-29T00:00:00Z"));
    }

    #[test]
    fn extract_message_text_prefers_text_field_in_object() {
        let message = serde_json::json!({
            "type": "text",
            "text": "<RUNTIME_INFORMATION>...",
            "image_bytes": null,
            "image_url": null,
            "input_audio": null,
            "media_type": null
        });
        assert_eq!(
            extract_message_text(&message),
            Some("<RUNTIME_INFORMATION>...".into())
        );
    }

    #[test]
    fn extract_message_text_joins_text_parts_in_array() {
        let message = serde_json::json!([
            {"type": "text", "text": "first"},
            {"type": "image", "image_url": {"url": "http://example.com/a.png"}},
            {"type": "text", "text": "second"}
        ]);
        assert_eq!(extract_message_text(&message), Some("first\nsecond".into()));
    }

    #[test]
    fn extract_message_text_falls_back_to_none_for_pure_objects() {
        let message = serde_json::json!({"foo": "bar"});
        assert_eq!(extract_message_text(&message), None);
    }

    fn kind_catalog() -> QueryCatalog {
        QueryCatalog {
            snapshot_id: "s".into(),
            read_only: true,
            database: "atif".into(),
            storage_path: "/tmp".into(),
            path_column: "_file_".into(),
            datasets: vec![
                QueryDatasetSummary {
                    name: "atif".into(),
                    uri: "atif".into(),
                    ready_sources: 1,
                    error_sources: 0,
                },
                QueryDatasetSummary {
                    name: "actf".into(),
                    uri: "actf".into(),
                    ready_sources: 1,
                    error_sources: 0,
                },
            ],
            tables: vec![
                QueryTableSummary {
                    name: "runs".into(),
                    description: "trajectories".into(),
                    kind: "table".into(),
                    grain: "run".into(),
                    fields: Vec::new(),
                },
                QueryTableSummary {
                    name: "steps".into(),
                    description: "steps".into(),
                    kind: "table".into(),
                    grain: "step".into(),
                    fields: Vec::new(),
                },
            ],
        }
    }

    #[test]
    fn queryable_tables_are_dataset_qualified_sql_names() {
        let names: Vec<_> = queryable_tables(&kind_catalog())
            .into_iter()
            .map(|table| table.name)
            .collect();
        assert_eq!(
            names,
            vec!["atif.runs", "atif.steps", "actf.runs", "actf.steps"]
        );
    }

    #[test]
    fn queryable_tables_use_database_when_datasets_are_missing() {
        let mut catalog = kind_catalog();
        catalog.datasets.clear();
        catalog.database = "dataset".into();
        let names: Vec<_> = queryable_tables(&catalog)
            .into_iter()
            .map(|table| table.name)
            .collect();
        assert_eq!(names, vec!["dataset.runs", "dataset.steps"]);
    }

    #[test]
    fn queryable_tables_keep_already_qualified_names() {
        let mut catalog = kind_catalog();
        catalog.tables[0].name = "default.runs".into();
        catalog.tables.truncate(1);
        let names: Vec<_> = queryable_tables(&catalog)
            .into_iter()
            .map(|table| table.name)
            .collect();
        assert_eq!(names, vec!["default.runs"]);
    }
}
