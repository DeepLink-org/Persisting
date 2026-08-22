//! Arrow codecs for the Storyline-native three-table Lance layout.

use std::sync::Arc;

use anyhow::{Context, Result};
use lance::deps::arrow_array::{
    Array, BooleanArray, Int64Array, RecordBatch, StringArray, TimestampMillisecondArray,
    TimestampNanosecondArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::super::storyline_model::{StoryRunRow, StoryStepRow, StoryToolCallRow};
use crate::model::StorylineTimestamp;

fn field(name: &str, data_type: DataType, nullable: bool) -> Field {
    Field::new(name, data_type, nullable)
}

pub fn story_runs_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("schema_version", DataType::Utf8, false),
        field("origin_json", DataType::Utf8, true),
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
        field("agent_tool_definitions_json", DataType::Utf8, true),
        field("agent_extra_json", DataType::Utf8, true),
        field("parent_json", DataType::Utf8, true),
        field("child_session_ids_json", DataType::Utf8, true),
        field("notes", DataType::Utf8, true),
        field("final_metrics_json", DataType::Utf8, true),
        field("continued_trajectory_ref", DataType::Utf8, true),
        field("extra_json", DataType::Utf8, true),
        field("meta_json", DataType::Utf8, true),
        field("unknown_fields_json", DataType::Utf8, true),
        field("unknown_key_counts_json", DataType::Utf8, true),
        field("task_json", DataType::Utf8, true),
        field("started_at_json", DataType::Utf8, true),
        field("finished_at_json", DataType::Utf8, true),
        field("prompt_json", DataType::Utf8, true),
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
        // Keep the authoritative JSON scalar for lossless string/number recovery.
        field("timestamp_source_json", DataType::Utf8, true),
        field("source", DataType::Utf8, false),
        field("message_json", DataType::Utf8, false),
        field("reasoning_content", DataType::Utf8, true),
        field("reasoning_effort_json", DataType::Utf8, true),
        field("metrics_json", DataType::Utf8, true),
        field("model_name", DataType::Utf8, true),
        field("llm_call_count", DataType::Int64, true),
        field("is_copied_context", DataType::Boolean, true),
        field("latency_ms", DataType::Int64, true),
        field("ttft_ms", DataType::Int64, true),
        field("had_tool_calls", DataType::Boolean, false),
        field("had_observation", DataType::Boolean, false),
        field("observation_json", DataType::Utf8, true),
        field("extra_json", DataType::Utf8, true),
        field("env_json", DataType::Utf8, true),
        field("finished_at_json", DataType::Utf8, true),
        field("prompt_json", DataType::Utf8, true),
    ]))
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
        field("arguments_json", DataType::Utf8, false),
        field("result_json", DataType::Utf8, true),
        field("results_json", DataType::Utf8, false),
        field("duration_ms", DataType::Int64, true),
        field("extra_json", DataType::Utf8, true),
        field("kind", DataType::Utf8, true),
        field("response_json", DataType::Utf8, true),
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
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.agent_extra))
                    .collect::<Result<Vec<_>>>()?,
            )),
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
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.final_metrics))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8(
                rows.iter().map(|r| r.continued_trajectory_ref.as_deref()),
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.meta))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| {
                        (!r.unknown_fields.is_empty())
                            .then(|| json(&r.unknown_fields))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>>>()?,
            )),
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
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.started_at))
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.finished_at))
                    .collect::<Result<Vec<_>>>()?,
            )),
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
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| {
                        r.timestamp
                            .as_ref()
                            .map(|timestamp| json(timestamp.source_value()))
                            .transpose()
                    })
                    .collect::<Result<Vec<_>>>()?,
            )),
            Arc::new(req_utf8(rows.iter().map(|r| r.source.as_str()))),
            Arc::new(req_utf8_owned(
                rows.iter()
                    .map(|r| json(&r.message))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8(
                rows.iter().map(|r| r.reasoning_content.as_deref()),
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.reasoning_effort))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.metrics))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.model_name.as_deref()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.llm_call_count).collect::<Vec<_>>(),
            )),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.is_copied_context).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.latency_ms).collect::<Vec<_>>(),
            )),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.ttft_ms).collect::<Vec<_>>(),
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
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.env))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.finished_at))
                    .collect::<Result<_>>()?,
            )),
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
                rows.iter().map(|r| r.duration_ms).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
                    .collect::<Result<_>>()?,
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.kind.as_deref()))),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.response))
                    .collect::<Result<_>>()?,
            )),
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
    let column = batch.column(column_index(batch, name)?);
    let array = column
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("expected Utf8 column '{name}'"))?;
    Ok((!array.is_null(row)).then(|| array.value(row).to_string()))
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
                || anyhow::anyhow!("legacy timestamp millisecond value is outside nanosecond range"),
            )?))
        }
        data_type => anyhow::bail!(
            "expected Timestamp(Nanosecond, UTC) or legacy Timestamp(Millisecond, UTC) column '{name}', got {data_type:?}"
        ),
    }
}

fn timestamp_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<StorylineTimestamp>> {
    let semantic_nanos = timestamp_nanos_at(batch, name, row)?;
    let source = match string_at_if_present(batch, "timestamp_source_json", row)? {
        Some(value) => Some(parse_json::<serde_json::Value>(
            value,
            "timestamp_source_json",
        )?),
        None => {
            string_at_if_present(batch, "timestamp_rfc3339", row)?.map(serde_json::Value::String)
        }
    };
    let Some(semantic_nanos) = semantic_nanos else {
        anyhow::ensure!(
            source.is_none(),
            "null timestamp has a non-null source scalar"
        );
        return Ok(None);
    };
    let timestamp = match source {
        Some(source) => StorylineTimestamp::from_json(source)?,
        None => StorylineTimestamp::from_utc(
            chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(semantic_nanos),
        )?,
    };
    anyhow::ensure!(
        timestamp.timestamp_nanos() == semantic_nanos,
        "timestamp semantic value disagrees with its source scalar"
    );
    Ok(Some(timestamp))
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
                origin: optional_json_at(batch, "origin_json", row)?,
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
                agent_tool_definitions: optional_json_at(
                    batch,
                    "agent_tool_definitions_json",
                    row,
                )?,
                agent_extra: optional_json_at(batch, "agent_extra_json", row)?,
                parent: optional_json_at(batch, "parent_json", row)?,
                child_session_ids: optional_json_at(batch, "child_session_ids_json", row)?,
                notes: string_at(batch, "notes", row)?,
                final_metrics: optional_json_at(batch, "final_metrics_json", row)?,
                continued_trajectory_ref: string_at(batch, "continued_trajectory_ref", row)?,
                extra: optional_json_at(batch, "extra_json", row)?,
                meta: optional_json_if_present(batch, "meta_json", row)?,
                unknown_fields: optional_json_if_present(batch, "unknown_fields_json", row)?
                    .unwrap_or_default(),
                unknown_key_counts: optional_json_if_present(
                    batch,
                    "unknown_key_counts_json",
                    row,
                )?
                .unwrap_or_default(),
                task: optional_json_if_present(batch, "task_json", row)?,
                started_at: optional_json_if_present(batch, "started_at_json", row)?,
                finished_at: optional_json_if_present(batch, "finished_at_json", row)?,
                prompt: optional_json_if_present(batch, "prompt_json", row)?,
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
                message: parse_json(
                    required_string_at(batch, "message_json", row)?,
                    "message_json",
                )?,
                reasoning_content: string_at(batch, "reasoning_content", row)?,
                reasoning_effort: optional_json_at(batch, "reasoning_effort_json", row)?,
                metrics: optional_json_at(batch, "metrics_json", row)?,
                model_name: string_at(batch, "model_name", row)?,
                llm_call_count: i64_at(batch, "llm_call_count", row)?,
                is_copied_context: bool_at(batch, "is_copied_context", row)?,
                latency_ms: i64_at(batch, "latency_ms", row)?,
                ttft_ms: i64_at(batch, "ttft_ms", row)?,
                had_tool_calls: required_bool_at(batch, "had_tool_calls", row)?,
                had_observation: required_bool_at(batch, "had_observation", row)?,
                observation: optional_json_at(batch, "observation_json", row)?,
                extra: optional_json_at(batch, "extra_json", row)?,
                env: optional_json_if_present(batch, "env_json", row)?,
                finished_at: optional_json_if_present(batch, "finished_at_json", row)?,
                prompt: optional_json_if_present(batch, "prompt_json", row)?,
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
                arguments: parse_json(
                    required_string_at(batch, "arguments_json", row)?,
                    "arguments_json",
                )?,
                result: string_at(batch, "result_json", row)?
                    .map(|value| parse_json(value, "result_json"))
                    .transpose()?,
                results: parse_json(
                    required_string_at(batch, "results_json", row)?,
                    "results_json",
                )?,
                duration_ms: i64_at(batch, "duration_ms", row)?,
                extra: optional_json_at(batch, "extra_json", row)?,
                kind: string_at_if_present(batch, "kind", row)?,
                response: optional_json_if_present(batch, "response_json", row)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batches_keep_all_three_schemas() {
        assert_eq!(story_runs_to_batch(&[]).unwrap().num_columns(), 27);
        assert_eq!(story_steps_to_batch(&[]).unwrap().num_columns(), 26);
        assert_eq!(story_tool_calls_to_batch(&[]).unwrap().num_columns(), 14);
    }

    #[test]
    fn step_timestamps_store_nanoseconds_and_preserve_source_scalars() {
        let schema = story_steps_arrow_schema();
        assert_eq!(
            schema.field_with_name("timestamp").unwrap().data_type(),
            &DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))
        );
        assert_eq!(
            schema
                .field_with_name("timestamp_source_json")
                .unwrap()
                .data_type(),
            &DataType::Utf8
        );

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
                latency_ms: None,
                ttft_ms: None,
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
            decoded[0].timestamp.as_ref().unwrap().source_value(),
            &serde_json::json!(1.25)
        );
        assert_eq!(
            decoded[1].timestamp.as_ref().unwrap().source_value(),
            &serde_json::json!("1970-01-01T00:00:01.250000000Z")
        );
    }

    #[test]
    fn step_timestamp_rejects_semantic_source_disagreement() {
        let rows = vec![StoryStepRow {
            document_id: "d".into(),
            run_id: Some("r".into()),
            session_id: "s".into(),
            step_id: 1,
            turn_ordinal: 0,
            kind: None,
            effective_kind: "dialogue".into(),
            timestamp: Some(
                crate::model::StorylineTimestamp::from_json(serde_json::json!(1.25)).unwrap(),
            ),
            source: "user".into(),
            message: serde_json::json!("hello"),
            reasoning_content: None,
            reasoning_effort: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            had_tool_calls: false,
            had_observation: false,
            observation: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        }];
        let batch = story_steps_to_batch(&rows).unwrap();
        let source_index = batch.schema().index_of("timestamp_source_json").unwrap();
        let mut columns = batch.columns().to_vec();
        columns[source_index] = Arc::new(StringArray::from(vec![Some("\"1970-01-01T00:00:02Z\"")]));
        let corrupt = RecordBatch::try_new(batch.schema(), columns).unwrap();

        let error = story_steps_from_batch(&corrupt).unwrap_err();
        assert!(error.to_string().contains("disagrees"), "{error:#}");
    }

    #[test]
    fn step_timestamp_reads_legacy_millisecond_and_rfc3339_columns() {
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
            latency_ms: None,
            ttft_ms: None,
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
        let source_index = current.schema().index_of("timestamp_source_json").unwrap();
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
        fields[source_index] = field("timestamp_rfc3339", DataType::Utf8, true);
        let schema = Arc::new(ArrowSchema::new(fields));
        let mut columns = current.columns().to_vec();
        columns[timestamp_index] =
            Arc::new(TimestampMillisecondArray::from(vec![Some(1_250)]).with_timezone("UTC"));
        columns[source_index] = Arc::new(StringArray::from(vec![Some("1970-01-01T00:00:01.250Z")]));
        let legacy = RecordBatch::try_new(schema, columns).unwrap();

        let decoded = story_steps_from_batch(&legacy).unwrap();
        let timestamp = decoded[0].timestamp.as_ref().unwrap();
        assert_eq!(timestamp.timestamp_nanos(), 1_250_000_000);
        assert_eq!(
            timestamp.source_value(),
            &serde_json::json!("1970-01-01T00:00:01.250Z")
        );
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
            duration_ms: Some(8),
            extra: None,
            kind: None,
            response: None,
        }];
        let batch = story_tool_calls_to_batch(&rows).unwrap();
        assert_eq!(story_tool_calls_from_batch(&batch).unwrap(), rows);
    }
}
