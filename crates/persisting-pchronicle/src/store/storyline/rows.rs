//! Arrow codecs for the Storyline-native three-table Lance layout.

use std::sync::Arc;

use anyhow::{Context, Result};
use lance::deps::arrow_array::{
    Array, BooleanArray, Int64Array, LargeBinaryArray, RecordBatch, StringArray,
    TimestampMillisecondArray, TimestampNanosecondArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::super::storyline_model::{StoryRunRow, StoryStepRow, StoryToolCallRow};
use crate::model::StorylineTimestamp;

fn field(name: &str, data_type: DataType, nullable: bool) -> Field {
    Field::new(name, data_type, nullable)
}

fn json_field(name: &str, nullable: bool) -> Field {
    lance_arrow::json::json_field(name, nullable)
}

pub fn story_runs_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("schema_version", DataType::Utf8, false),
        field("origin", DataType::Utf8, true),
        field("document_id", DataType::Utf8, false),
        field("storage_ordinal", DataType::Int64, false),
        field("trajectory_id_explicit", DataType::Boolean, false),
        field("run_id", DataType::Utf8, true),
        field("attempt_id", DataType::Utf8, true),
        field("session_id", DataType::Utf8, false),
        field("agent_id", DataType::Utf8, false),
        field("agent_name", DataType::Utf8, true),
        field("agent_version", DataType::Utf8, true),
        field("agent_model_name", DataType::Utf8, true),
        field("agent_tool_definitions", DataType::Utf8, true),
        json_field("agent_extra", true),
        field("parent", DataType::Utf8, true),
        field("child_session_ids", DataType::Utf8, true),
        field("notes", DataType::Utf8, true),
        json_field("final_metrics", true),
        field("continued_trajectory_ref", DataType::Utf8, true),
        json_field("extra", true),
        json_field("meta", true),
        json_field("unknown_fields", true),
        field("unknown_key_counts", DataType::Utf8, true),
        field("task", DataType::Utf8, true),
        field(
            "started_at",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        field(
            "finished_at",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        field("prompt", DataType::Utf8, true),
    ]))
}

pub fn story_steps_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("document_id", DataType::Utf8, false),
        field("run_id", DataType::Utf8, true),
        field("session_id", DataType::Utf8, false),
        field("step_id", DataType::Int64, false),
        field("turn_ordinal", DataType::Int64, false),
        field("kind", DataType::Utf8, true),
        field("effective_kind", DataType::Utf8, false),
        field(
            "timestamp",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        field("source", DataType::Utf8, false),
        field("message_kind", DataType::Utf8, false),
        field("message_value", DataType::Utf8, false),
        field("reasoning_content", DataType::Utf8, true),
        field("reasoning_effort_kind", DataType::Utf8, true),
        field("reasoning_effort_value", DataType::Utf8, true),
        json_field("metrics", true),
        field("model_name", DataType::Utf8, true),
        field("llm_call_count", DataType::Int64, true),
        field("is_copied_context", DataType::Boolean, true),
        field("latency", DataType::Int64, true),
        field("ttft", DataType::Int64, true),
        field("had_tool_calls", DataType::Boolean, false),
        field("had_observation", DataType::Boolean, false),
        field("observation", DataType::Utf8, true),
        json_field("extra", true),
        field("env", DataType::Utf8, true),
        field(
            "finished_at",
            DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into())),
            true,
        ),
        field("prompt", DataType::Utf8, true),
    ]))
}

/// Logical variants used by the Storyline message storage lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Null,
    Text,
    Parts,
    Json,
}

impl MessageKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Text => "text",
            Self::Parts => "parts",
            Self::Json => "json",
        }
    }
}

impl std::fmt::Display for MessageKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for MessageKind {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "null" => Ok(Self::Null),
            "text" => Ok(Self::Text),
            "parts" => Ok(Self::Parts),
            "json" => Ok(Self::Json),
            other => anyhow::bail!("unknown message_kind '{other}'"),
        }
    }
}

/// Stable logical type tag for the lossless `message_value` JSON payload.
///
/// `message_value` remains the canonical representation so arbitrary producer
/// fields are preserved, while this tag lets query consumers distinguish the
/// common text/parts forms without parsing every value.
pub fn message_kind_for_value(value: &serde_json::Value) -> MessageKind {
    match value {
        serde_json::Value::Null => MessageKind::Null,
        serde_json::Value::String(_) => MessageKind::Text,
        serde_json::Value::Array(_) => MessageKind::Parts,
        serde_json::Value::Object(_)
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => MessageKind::Json,
    }
}

/// Logical variants used by the optional reasoning-effort scalar lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffortKind {
    Null,
    Text,
    Number,
    Json,
}

impl ReasoningEffortKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Text => "text",
            Self::Number => "number",
            Self::Json => "json",
        }
    }
}

impl TryFrom<&str> for ReasoningEffortKind {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "null" => Ok(Self::Null),
            "text" => Ok(Self::Text),
            "number" => Ok(Self::Number),
            "json" => Ok(Self::Json),
            other => anyhow::bail!("unknown reasoning_effort_kind '{other}'"),
        }
    }
}

pub fn reasoning_effort_kind_for_value(value: &serde_json::Value) -> ReasoningEffortKind {
    match value {
        serde_json::Value::Null => ReasoningEffortKind::Null,
        serde_json::Value::String(_) => ReasoningEffortKind::Text,
        serde_json::Value::Number(_) => ReasoningEffortKind::Number,
        serde_json::Value::Array(_) | serde_json::Value::Object(_) | serde_json::Value::Bool(_) => {
            ReasoningEffortKind::Json
        }
    }
}

pub fn story_tool_calls_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("document_id", DataType::Utf8, false),
        field("run_id", DataType::Utf8, true),
        field("session_id", DataType::Utf8, false),
        field("step_id", DataType::Int64, false),
        field("call_index", DataType::Int64, false),
        field("tool_call_id", DataType::Utf8, false),
        field("function_name", DataType::Utf8, false),
        field("arguments", DataType::Utf8, false),
        field("result", DataType::Utf8, true),
        field("results", DataType::Utf8, false),
        field("duration", DataType::Int64, true),
        json_field("extra", true),
        field("kind", DataType::Utf8, true),
        json_field("response", true),
    ]))
}

fn req_utf8<'a>(values: impl IntoIterator<Item = &'a str>) -> StringArray {
    StringArray::from_iter_values(values)
}

fn opt_utf8<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> StringArray {
    StringArray::from_iter(values)
}

fn req_utf8_owned(values: Vec<String>) -> StringArray {
    req_utf8(values.iter().map(String::as_str))
}

fn opt_utf8_owned(values: Vec<Option<String>>) -> StringArray {
    opt_utf8(values.iter().map(Option::as_deref))
}

pub(crate) fn timestamp_array<'a>(
    values: impl IntoIterator<Item = Option<&'a StorylineTimestamp>>,
) -> TimestampNanosecondArray {
    TimestampNanosecondArray::from(
        values
            .into_iter()
            .map(|value| value.map(StorylineTimestamp::timestamp_nanos))
            .collect::<Vec<_>>(),
    )
    .with_timezone("UTC")
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).context("serialize Storyline Lance JSON column")
}

fn opt_json<T: Serialize>(value: &Option<T>) -> Result<Option<String>> {
    value.as_ref().map(json).transpose()
}

fn json_array_owned(values: Vec<Option<String>>) -> Result<LargeBinaryArray> {
    lance_arrow::json::JsonArray::try_from_iter(values)
        .map(|array| array.into_inner())
        .context("encode Lance JSON column")
}

pub fn story_runs_to_batch(rows: &[StoryRunRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        story_runs_arrow_schema(),
        vec![
            Arc::new(req_utf8(rows.iter().map(|r| r.schema_version.as_str()))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.origin))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(req_utf8(rows.iter().map(|r| r.document_id.as_str()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.storage_ordinal).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|r| r.trajectory_id_explicit)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.run_id.as_deref()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.attempt_id.as_deref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.session_id.as_str()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.agent_id.as_str()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.agent_name.as_deref()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.agent_version.as_deref()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.agent_model_name.as_deref()))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.agent_tool_definitions))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.agent_extra))
                    .collect::<Result<Vec<_>>>()?,
            )?),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.parent))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.child_session_ids))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.notes.as_deref()))),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.final_metrics))
                    .collect::<Result<Vec<_>>>()?,
            )?),
            Arc::new(opt_utf8(
                rows.iter().map(|r| r.continued_trajectory_ref.as_deref()),
            )),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<Vec<_>>>()?,
            )?),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.meta))
                    .collect::<Result<Vec<_>>>()?,
            )?),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| {
                        (!r.unknown_fields.is_empty())
                            .then(|| json(&r.unknown_fields))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>>>()?,
            )?),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| {
                        (!r.unknown_key_counts.is_empty())
                            .then(|| json(&r.unknown_key_counts))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.task))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(timestamp_array(rows.iter().map(|r| r.started_at.as_ref()))),
            Arc::new(timestamp_array(rows.iter().map(|r| r.finished_at.as_ref()))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.prompt))
                    .collect::<Result<Vec<_>>>()?,
            )),
        ],
    )
    .context("build runs Lance batch")
}

pub fn story_steps_to_batch(rows: &[StoryStepRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        story_steps_arrow_schema(),
        vec![
            Arc::new(req_utf8(rows.iter().map(|r| r.document_id.as_str()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.run_id.as_deref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.session_id.as_str()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.step_id).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.turn_ordinal).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.kind.as_deref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.effective_kind.as_str()))),
            Arc::new(timestamp_array(rows.iter().map(|r| r.timestamp.as_ref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.source.as_str()))),
            Arc::new(req_utf8(
                rows.iter()
                    .map(|r| message_kind_for_value(&r.message).as_str()),
            )),
            Arc::new(req_utf8_owned(
                rows.iter()
                    .map(|r| json(&r.message))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8(
                rows.iter().map(|r| r.reasoning_content.as_deref()),
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| {
                r.reasoning_effort
                    .as_ref()
                    .map(|value| reasoning_effort_kind_for_value(value).as_str())
            }))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.reasoning_effort))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.metrics))
                    .collect::<Result<_>>()?,
            )?),
            Arc::new(opt_utf8(rows.iter().map(|r| r.model_name.as_deref()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.llm_call_count).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.is_copied_context).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.latency).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.ttft).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.had_tool_calls).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.had_observation).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.observation))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<_>>()?,
            )?),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.env))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(timestamp_array(rows.iter().map(|r| r.finished_at.as_ref()))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.prompt))
                    .collect::<Result<_>>()?,
            )),
        ],
    )
    .context("build steps Lance batch")
}

pub fn story_tool_calls_to_batch(rows: &[StoryToolCallRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        story_tool_calls_arrow_schema(),
        vec![
            Arc::new(req_utf8(rows.iter().map(|r| r.document_id.as_str()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.run_id.as_deref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.session_id.as_str()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.step_id).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.call_index).collect::<Vec<_>>(),
            )),
            Arc::new(req_utf8(rows.iter().map(|r| r.tool_call_id.as_str()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.function_name.as_str()))),
            Arc::new(req_utf8_owned(
                rows.iter()
                    .map(|r| json(&r.arguments))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| r.result.as_ref().map(json).transpose())
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(req_utf8_owned(
                rows.iter()
                    .map(|r| json(&r.results))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.duration).collect::<Vec<_>>(),
            )),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<_>>()?,
            )?),
            Arc::new(opt_utf8(rows.iter().map(|r| r.kind.as_deref()))),
            Arc::new(json_array_owned(
                rows.iter()
                    .map(|r| opt_json(&r.response))
                    .collect::<Result<_>>()?,
            )?),
        ],
    )
    .context("build tool_calls Lance batch")
}

fn column_index(batch: &RecordBatch, name: &str) -> Result<usize> {
    batch
        .schema()
        .fields()
        .iter()
        .position(|f| f.name() == name)
        .ok_or_else(|| anyhow::anyhow!("batch missing column '{name}'"))
}

fn string_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>> {
    let index = column_index(batch, name)?;
    let column = batch.column(index);
    if let Some(array) = column.as_any().downcast_ref::<StringArray>() {
        return Ok((!array.is_null(row)).then(|| array.value(row).to_string()));
    }
    if let Some(array) = column.as_any().downcast_ref::<LargeBinaryArray>() {
        anyhow::ensure!(
            lance_arrow::json::is_json_field(batch.schema().field(index)),
            "expected Utf8 column '{name}'"
        );
        return Ok((!array.is_null(row)).then(|| lance_arrow::json::decode_json(array.value(row))));
    }
    anyhow::bail!("expected Utf8 or Lance JSON column '{name}'")
}

fn string_at_if_present(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>> {
    if batch.schema().field_with_name(name).is_err() {
        return Ok(None);
    }
    string_at(batch, name, row)
}

fn required_string_at(batch: &RecordBatch, name: &str, row: usize) -> Result<String> {
    string_at(batch, name, row)?.ok_or_else(|| anyhow::anyhow!("null required column '{name}'"))
}

fn i64_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<i64>> {
    let column = batch.column(column_index(batch, name)?);
    let array = column
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow::anyhow!("expected Int64 column '{name}'"))?;
    Ok((!array.is_null(row)).then(|| array.value(row)))
}

fn required_i64_at(batch: &RecordBatch, name: &str, row: usize) -> Result<i64> {
    i64_at(batch, name, row)?.ok_or_else(|| anyhow::anyhow!("null required column '{name}'"))
}

fn timestamp_nanos_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<i64>> {
    let column = batch.column(column_index(batch, name)?);
    match column.data_type() {
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let array = column
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .ok_or_else(|| anyhow::anyhow!("invalid nanosecond timestamp column '{name}'"))?;
            Ok((!array.is_null(row)).then(|| array.value(row)))
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let array = column
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .ok_or_else(|| anyhow::anyhow!("invalid millisecond timestamp column '{name}'"))?;
            if array.is_null(row) {
                return Ok(None);
            }
            Ok(Some(array.value(row).checked_mul(1_000_000).ok_or_else(
                || {
                    anyhow::anyhow!(
                        "legacy timestamp millisecond value is outside nanosecond range"
                    )
                },
            )?))
        }
        data_type => anyhow::bail!(
            "expected Timestamp(Nanosecond, UTC) or legacy Timestamp(Millisecond, UTC) column '{name}', got {data_type:?}"
        ),
    }
}

fn timestamp_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<StorylineTimestamp>> {
    let semantic_nanos = timestamp_nanos_at(batch, name, row)?;
    let Some(semantic_nanos) = semantic_nanos else {
        return Ok(None);
    };
    Ok(Some(StorylineTimestamp::from_utc(chrono::DateTime::<
        chrono::Utc,
    >::from_timestamp_nanos(
        semantic_nanos
    ))?))
}

fn bool_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<bool>> {
    let column = batch.column(column_index(batch, name)?);
    let array = column
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow::anyhow!("expected Boolean column '{name}'"))?;
    Ok((!array.is_null(row)).then(|| array.value(row)))
}

fn required_bool_at(batch: &RecordBatch, name: &str, row: usize) -> Result<bool> {
    bool_at(batch, name, row)?.ok_or_else(|| anyhow::anyhow!("null required column '{name}'"))
}

fn parse_json<T: DeserializeOwned>(value: String, name: &str) -> Result<T> {
    serde_json::from_str(&value).with_context(|| format!("parse JSON column '{name}'"))
}

fn optional_json_at<T: DeserializeOwned>(
    batch: &RecordBatch,
    name: &str,
    row: usize,
) -> Result<Option<T>> {
    string_at(batch, name, row)?
        .map(|value| parse_json(value, name))
        .transpose()
}

fn optional_json_if_present<T: DeserializeOwned>(
    batch: &RecordBatch,
    name: &str,
    row: usize,
) -> Result<Option<T>> {
    string_at_if_present(batch, name, row)?
        .map(|value| parse_json(value, name))
        .transpose()
}

pub fn story_runs_from_batch(batch: &RecordBatch) -> Result<Vec<StoryRunRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryRunRow {
                schema_version: required_string_at(batch, "schema_version", row)?,
                origin: optional_json_at(batch, "origin", row)?,
                document_id: required_string_at(batch, "document_id", row)?,
                storage_ordinal: required_i64_at(batch, "storage_ordinal", row)?,
                trajectory_id_explicit: required_bool_at(batch, "trajectory_id_explicit", row)?,
                run_id: string_at(batch, "run_id", row)?,
                attempt_id: string_at(batch, "attempt_id", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                agent_id: required_string_at(batch, "agent_id", row)?,
                agent_name: string_at(batch, "agent_name", row)?,
                agent_version: string_at(batch, "agent_version", row)?,
                agent_model_name: string_at(batch, "agent_model_name", row)?,
                agent_tool_definitions: optional_json_at(batch, "agent_tool_definitions", row)?,
                agent_extra: optional_json_at(batch, "agent_extra", row)?,
                parent: optional_json_at(batch, "parent", row)?,
                child_session_ids: optional_json_at(batch, "child_session_ids", row)?,
                notes: string_at(batch, "notes", row)?,
                final_metrics: optional_json_at(batch, "final_metrics", row)?,
                continued_trajectory_ref: string_at(batch, "continued_trajectory_ref", row)?,
                extra: optional_json_at(batch, "extra", row)?,
                meta: optional_json_if_present(batch, "meta", row)?,
                unknown_fields: optional_json_if_present(batch, "unknown_fields", row)?
                    .unwrap_or_default(),
                unknown_key_counts: optional_json_if_present(batch, "unknown_key_counts", row)?
                    .unwrap_or_default(),
                task: optional_json_if_present(batch, "task", row)?,
                started_at: timestamp_at(batch, "started_at", row)?,
                finished_at: timestamp_at(batch, "finished_at", row)?,
                prompt: optional_json_if_present(batch, "prompt", row)?,
            })
        })
        .collect()
}

pub fn story_steps_from_batch(batch: &RecordBatch) -> Result<Vec<StoryStepRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryStepRow {
                document_id: required_string_at(batch, "document_id", row)?,
                run_id: string_at(batch, "run_id", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                step_id: required_i64_at(batch, "step_id", row)?,
                turn_ordinal: required_i64_at(batch, "turn_ordinal", row)?,
                kind: string_at(batch, "kind", row)?,
                effective_kind: required_string_at(batch, "effective_kind", row)?,
                timestamp: timestamp_at(batch, "timestamp", row)?,
                source: required_string_at(batch, "source", row)?,
                message: {
                    if batch.schema().field_with_name("message_value").is_ok() {
                        let value = parse_json(
                            required_string_at(batch, "message_value", row)?,
                            "message_value",
                        )?;
                        let kind = MessageKind::try_from(
                            required_string_at(batch, "message_kind", row)?.as_str(),
                        )?;
                        anyhow::ensure!(
                            kind == message_kind_for_value(&value),
                            "message_kind does not match message_value at row {row}"
                        );
                        value
                    } else {
                        parse_json(
                            required_string_at(batch, "message_json", row)?,
                            "message_json",
                        )?
                    }
                },
                reasoning_content: string_at(batch, "reasoning_content", row)?,
                reasoning_effort: {
                    if batch
                        .schema()
                        .field_with_name("reasoning_effort_value")
                        .is_ok()
                    {
                        let kind = string_at_if_present(batch, "reasoning_effort_kind", row)?;
                        let value = optional_json_if_present(batch, "reasoning_effort_value", row)?;
                        match (kind, value) {
                            (None, None) => None,
                            (Some(kind), Some(value)) => {
                                let kind = ReasoningEffortKind::try_from(kind.as_str())?;
                                anyhow::ensure!(
                                    kind == reasoning_effort_kind_for_value(&value),
                                    "reasoning_effort_kind does not match reasoning_effort_value at row {row}"
                                );
                                Some(value)
                            }
                            _ => anyhow::bail!(
                                "reasoning_effort_kind and reasoning_effort_value must be present together at row {row}"
                            ),
                        }
                    } else {
                        optional_json_if_present(batch, "reasoning_effort_json", row)?
                    }
                },
                metrics: optional_json_at(batch, "metrics", row)?,
                model_name: string_at(batch, "model_name", row)?,
                llm_call_count: i64_at(batch, "llm_call_count", row)?,
                is_copied_context: bool_at(batch, "is_copied_context", row)?,
                latency: i64_at(batch, "latency", row)?,
                ttft: i64_at(batch, "ttft", row)?,
                had_tool_calls: required_bool_at(batch, "had_tool_calls", row)?,
                had_observation: required_bool_at(batch, "had_observation", row)?,
                observation: optional_json_at(batch, "observation", row)?,
                extra: optional_json_at(batch, "extra", row)?,
                env: optional_json_if_present(batch, "env", row)?,
                finished_at: timestamp_at(batch, "finished_at", row)?,
                prompt: optional_json_if_present(batch, "prompt", row)?,
            })
        })
        .collect()
}

pub fn story_tool_calls_from_batch(batch: &RecordBatch) -> Result<Vec<StoryToolCallRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryToolCallRow {
                document_id: required_string_at(batch, "document_id", row)?,
                run_id: string_at(batch, "run_id", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                step_id: required_i64_at(batch, "step_id", row)?,
                call_index: required_i64_at(batch, "call_index", row)?,
                tool_call_id: required_string_at(batch, "tool_call_id", row)?,
                function_name: required_string_at(batch, "function_name", row)?,
                arguments: parse_json(required_string_at(batch, "arguments", row)?, "arguments")?,
                result: string_at(batch, "result", row)?
                    .map(|value| parse_json(value, "result"))
                    .transpose()?,
                results: parse_json(required_string_at(batch, "results", row)?, "results")?,
                duration: i64_at(batch, "duration", row)?,
                extra: optional_json_at(batch, "extra", row)?,
                kind: string_at_if_present(batch, "kind", row)?,
                response: optional_json_if_present(batch, "response", row)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::storyline_model::split_storyline;

    #[test]
    fn empty_batches_keep_all_three_schemas() {
        assert_eq!(story_runs_to_batch(&[]).unwrap().num_columns(), 27);
        assert_eq!(story_steps_to_batch(&[]).unwrap().num_columns(), 27);
        assert_eq!(story_tool_calls_to_batch(&[]).unwrap().num_columns(), 14);
    }

    #[test]
    fn message_kind_separates_text_parts_and_escape_values() {
        assert_eq!(
            message_kind_for_value(&serde_json::json!("hello")),
            MessageKind::Text
        );
        assert_eq!(
            message_kind_for_value(&serde_json::json!([])),
            MessageKind::Parts
        );
        assert_eq!(
            message_kind_for_value(&serde_json::json!({"text": "hello"})),
            MessageKind::Json
        );
        assert_eq!(
            message_kind_for_value(&serde_json::Value::Null),
            MessageKind::Null
        );
        assert_eq!(
            reasoning_effort_kind_for_value(&serde_json::json!("medium")),
            ReasoningEffortKind::Text
        );
        assert_eq!(
            reasoning_effort_kind_for_value(&serde_json::json!(0.5)),
            ReasoningEffortKind::Number
        );
    }

    #[test]
    fn step_timestamps_store_nanoseconds() {
        let schema = story_steps_arrow_schema();
        assert_eq!(
            schema.field_with_name("timestamp").unwrap().data_type(),
            &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
        );
        assert!(schema.field_with_name("timestamp_source_json").is_err());
        assert!(schema.field_with_name("latency").is_ok());
        assert!(schema.field_with_name("ttft").is_ok());
        assert!(schema.field_with_name("latency_ms").is_err());
        assert!(schema.field_with_name("ttft_ms").is_err());
        let tool_schema = story_tool_calls_arrow_schema();
        assert!(tool_schema.field_with_name("duration").is_ok());
        assert!(tool_schema.field_with_name("duration_ms").is_err());

        let mut rows = Vec::new();
        for (step_id, timestamp) in [
            (1, serde_json::json!(1.25)),
            (2, serde_json::json!("1970-01-01T00:00:01.250000000Z")),
        ] {
            rows.push(StoryStepRow {
                document_id: "d".into(),
                run_id: Some("r".into()),
                session_id: "s".into(),
                step_id,
                turn_ordinal: step_id - 1,
                kind: None,
                effective_kind: "dialogue".into(),
                timestamp: Some(crate::model::StorylineTimestamp::from_json(timestamp).unwrap()),
                source: "user".into(),
                message: serde_json::json!("hello"),
                reasoning_content: None,
                reasoning_effort: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency: None,
                ttft: None,
                had_tool_calls: false,
                had_observation: false,
                observation: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            });
        }

        let batch = story_steps_to_batch(&rows).unwrap();
        let normalized = batch
            .column_by_name("timestamp")
            .unwrap()
            .as_any()
            .downcast_ref::<lance::deps::arrow_array::TimestampNanosecondArray>()
            .unwrap();
        assert_eq!(normalized.value(0), normalized.value(1));
        let decoded = story_steps_from_batch(&batch).unwrap();
        assert_eq!(
            decoded[0].timestamp.as_ref().unwrap().timestamp_nanos(),
            1_250_000_000
        );
        assert_eq!(
            decoded[1].timestamp.as_ref().unwrap().timestamp_nanos(),
            1_250_000_000
        );
    }

    #[test]
    fn json_columns_use_lance_json_extension_and_roundtrip() {
        let story = crate::StorylineDocument::new("session", "agent");
        let tables = split_storyline(&story).unwrap();
        let batch = story_runs_to_batch(std::slice::from_ref(&tables.run)).unwrap();
        let schema = batch.schema();
        for name in [
            "agent_extra",
            "final_metrics",
            "extra",
            "meta",
            "unknown_fields",
        ] {
            let field = schema.field_with_name(name).unwrap();
            assert!(lance_arrow::json::is_json_field(field), "{name}");
        }
        assert_eq!(story_runs_from_batch(&batch).unwrap()[0], tables.run);
    }

    #[test]
    fn step_timestamp_reads_legacy_millisecond_column() {
        let rows = vec![StoryStepRow {
            document_id: "d".into(),
            run_id: Some("r".into()),
            session_id: "s".into(),
            step_id: 1,
            turn_ordinal: 0,
            kind: None,
            effective_kind: "dialogue".into(),
            timestamp: None,
            source: "user".into(),
            message: serde_json::json!("hello"),
            reasoning_content: None,
            reasoning_effort: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency: None,
            ttft: None,
            had_tool_calls: false,
            had_observation: false,
            observation: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        }];
        let current = story_steps_to_batch(&rows).unwrap();
        let timestamp_index = current.schema().index_of("timestamp").unwrap();
        let mut fields = current
            .schema()
            .fields()
            .iter()
            .map(|field| field.as_ref().clone())
            .collect::<Vec<_>>();
        fields[timestamp_index] = field(
            "timestamp",
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            true,
        );
        let schema = Arc::new(ArrowSchema::new(fields));
        let mut columns = current.columns().to_vec();
        columns[timestamp_index] =
            Arc::new(TimestampMillisecondArray::from(vec![Some(1_250)]).with_timezone("UTC"));
        let legacy = RecordBatch::try_new(schema, columns).unwrap();

        let decoded = story_steps_from_batch(&legacy).unwrap();
        let timestamp = decoded[0].timestamp.as_ref().unwrap();
        assert_eq!(timestamp.timestamp_nanos(), 1_250_000_000);
    }

    #[test]
    fn tool_call_batch_round_trip() {
        let rows = vec![StoryToolCallRow {
            document_id: "d".into(),
            run_id: Some("r".into()),
            session_id: "s".into(),
            step_id: 2,
            call_index: 0,
            tool_call_id: "c".into(),
            function_name: "lookup".into(),
            arguments: serde_json::json!({"q": "x"}),
            result: None,
            results: vec![serde_json::json!({"source_call_id": "c", "content": "y"})],
            duration: Some(8),
            extra: None,
            kind: None,
            response: None,
        }];
        let batch = story_tool_calls_to_batch(&rows).unwrap();
        assert_eq!(story_tool_calls_from_batch(&batch).unwrap(), rows);
    }
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    fn token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9._-]{1,24}").unwrap()
    }

    proptest! {
        #[test]
        fn step_rows_roundtrip_through_arrow_without_losing_order_or_presence(
            rows in proptest::collection::vec(
                (token_strategy(), token_strategy(), proptest::string::string_regex("[A-Za-z0-9 .,!?]{0,48}").unwrap(), any::<bool>()),
                0..12,
            ),
        ) {
            let rows = rows.into_iter().enumerate().map(|(ordinal, (document_id, session_id, text, had_tool_calls))| {
                StoryStepRow {
                    document_id,
                    run_id: Some("run".into()),
                    session_id,
                    step_id: ordinal as i64 + 1,
                    turn_ordinal: ordinal as i64,
                    kind: Some("llm.response".into()),
                    effective_kind: "llm.response".into(),
                    timestamp: None,
                    source: "agent".into(),
                    message: json!({"text": text}),
                    reasoning_content: None,
                    reasoning_effort: None,
                    metrics: None,
                    model_name: Some("model".into()),
                    llm_call_count: Some(1),
                    is_copied_context: Some(false),
                    latency: Some(ordinal as i64),
                    ttft: None,
                    had_tool_calls,
                    had_observation: false,
                    observation: None,
                    extra: None,
                    env: None,
                    prompt: None,
                    finished_at: None,
                }
            }).collect::<Vec<_>>();

            let batch = story_steps_to_batch(&rows).expect("generated rows encode");
            let decoded = story_steps_from_batch(&batch).expect("generated rows decode");
            prop_assert_eq!(decoded, rows);
        }

        #[test]
        fn empty_or_nonempty_step_batches_keep_the_declared_schema(
            count in 0usize..16,
        ) {
            let rows = (0..count).map(|ordinal| StoryStepRow {
                document_id: format!("d-{ordinal}"),
                run_id: None,
                session_id: "s".into(),
                step_id: ordinal as i64 + 1,
                turn_ordinal: ordinal as i64,
                kind: None,
                effective_kind: "dialogue".into(),
                timestamp: None,
                source: "user".into(),
                message: json!("hello"),
                reasoning_content: None,
                reasoning_effort: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency: None,
                ttft: None,
                had_tool_calls: false,
                had_observation: false,
                observation: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            }).collect::<Vec<_>>();
            let batch = story_steps_to_batch(&rows).expect("rows encode");
            prop_assert_eq!(batch.num_rows(), count);
            prop_assert_eq!(batch.schema(), story_steps_arrow_schema());
        }

        #[test]
        fn step_batch_row_count_is_exact_for_generated_lengths(
            count in 0usize..32,
        ) {
            let rows = (0..count).map(|ordinal| StoryStepRow {
                document_id: format!("doc-{ordinal}"),
                run_id: None,
                session_id: "session".into(),
                step_id: ordinal as i64,
                turn_ordinal: ordinal as i64,
                kind: None,
                effective_kind: "dialogue".into(),
                timestamp: None,
                source: "agent".into(),
                message: json!("text"),
                reasoning_content: None,
                reasoning_effort: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency: None,
                ttft: None,
                had_tool_calls: false,
                had_observation: false,
                observation: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            }).collect::<Vec<_>>();
            let batch = story_steps_to_batch(&rows).expect("generated rows encode");
            prop_assert_eq!(batch.num_rows(), rows.len());
        }
    }
}
