//! Arrow codecs for the Storyline-native three-table Lance layout.

use std::sync::Arc;

use anyhow::{Context, Result};
use lance::deps::arrow_array::{Array, BooleanArray, Int64Array, RecordBatch, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::storyline_schema::{StoryRunRow, StoryStepRow, StoryToolCallRow};

fn field(name: &str, data_type: DataType, nullable: bool) -> Field {
    Field::new(name, data_type, nullable)
}

pub fn story_runs_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("run_id", DataType::Utf8, false),
        field("run_id_explicit", DataType::Boolean, false),
        field("session_id", DataType::Utf8, false),
        field("schema_version", DataType::Utf8, false),
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
    ]))
}

pub fn story_steps_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("run_id", DataType::Utf8, false),
        field("session_id", DataType::Utf8, false),
        field("step_id", DataType::Int64, false),
        field("kind", DataType::Utf8, true),
        field("effective_kind", DataType::Utf8, false),
        field("timestamp", DataType::Utf8, true),
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
        field("had_observation", DataType::Boolean, false),
        field("extra_json", DataType::Utf8, true),
    ]))
}

pub fn story_tool_calls_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        field("run_id", DataType::Utf8, false),
        field("session_id", DataType::Utf8, false),
        field("step_id", DataType::Int64, false),
        field("call_index", DataType::Int64, false),
        field("tool_call_id", DataType::Utf8, false),
        field("function_name", DataType::Utf8, false),
        field("arguments_json", DataType::Utf8, false),
        field("results_json", DataType::Utf8, false),
        field("duration_ms", DataType::Int64, true),
        field("extra_json", DataType::Utf8, true),
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
            Arc::new(req_utf8(rows.iter().map(|r| r.run_id.as_str()))),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.run_id_explicit).collect::<Vec<_>>(),
            )),
            Arc::new(req_utf8(rows.iter().map(|r| r.session_id.as_str()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.schema_version.as_str()))),
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
        ],
    )
    .context("build runs Lance batch")
}

pub fn story_steps_to_batch(rows: &[StoryStepRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        story_steps_arrow_schema(),
        vec![
            Arc::new(req_utf8(rows.iter().map(|r| r.run_id.as_str()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.session_id.as_str()))),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.step_id).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(rows.iter().map(|r| r.kind.as_deref()))),
            Arc::new(req_utf8(rows.iter().map(|r| r.effective_kind.as_str()))),
            Arc::new(opt_utf8(rows.iter().map(|r| r.timestamp.as_deref()))),
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
                rows.iter().map(|r| r.had_observation).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8_owned(
                rows.iter()
                    .map(|r| opt_json(&r.extra))
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
            Arc::new(req_utf8(rows.iter().map(|r| r.run_id.as_str()))),
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

pub fn story_runs_from_batch(batch: &RecordBatch) -> Result<Vec<StoryRunRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryRunRow {
                run_id: required_string_at(batch, "run_id", row)?,
                run_id_explicit: required_bool_at(batch, "run_id_explicit", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                schema_version: required_string_at(batch, "schema_version", row)?,
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
            })
        })
        .collect()
}

pub fn story_steps_from_batch(batch: &RecordBatch) -> Result<Vec<StoryStepRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryStepRow {
                run_id: required_string_at(batch, "run_id", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                step_id: required_i64_at(batch, "step_id", row)?,
                kind: string_at(batch, "kind", row)?,
                effective_kind: required_string_at(batch, "effective_kind", row)?,
                timestamp: string_at(batch, "timestamp", row)?,
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
                had_observation: required_bool_at(batch, "had_observation", row)?,
                extra: optional_json_at(batch, "extra_json", row)?,
            })
        })
        .collect()
}

pub fn story_tool_calls_from_batch(batch: &RecordBatch) -> Result<Vec<StoryToolCallRow>> {
    (0..batch.num_rows())
        .map(|row| {
            Ok(StoryToolCallRow {
                run_id: required_string_at(batch, "run_id", row)?,
                session_id: required_string_at(batch, "session_id", row)?,
                step_id: required_i64_at(batch, "step_id", row)?,
                call_index: required_i64_at(batch, "call_index", row)?,
                tool_call_id: required_string_at(batch, "tool_call_id", row)?,
                function_name: required_string_at(batch, "function_name", row)?,
                arguments: parse_json(
                    required_string_at(batch, "arguments_json", row)?,
                    "arguments_json",
                )?,
                results: parse_json(
                    required_string_at(batch, "results_json", row)?,
                    "results_json",
                )?,
                duration_ms: i64_at(batch, "duration_ms", row)?,
                extra: optional_json_at(batch, "extra_json", row)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_batches_keep_all_three_schemas() {
        assert_eq!(story_runs_to_batch(&[]).unwrap().num_columns(), 16);
        assert_eq!(story_steps_to_batch(&[]).unwrap().num_columns(), 18);
        assert_eq!(story_tool_calls_to_batch(&[]).unwrap().num_columns(), 10);
    }

    #[test]
    fn tool_call_batch_round_trip() {
        let rows = vec![StoryToolCallRow {
            run_id: "r".into(),
            session_id: "s".into(),
            step_id: 2,
            call_index: 0,
            tool_call_id: "c".into(),
            function_name: "lookup".into(),
            arguments: serde_json::json!({"q": "x"}),
            results: vec![serde_json::json!({"source_call_id": "c", "content": "y"})],
            duration_ms: Some(8),
            extra: None,
        }];
        let batch = story_tool_calls_to_batch(&rows).unwrap();
        assert_eq!(story_tool_calls_from_batch(&batch).unwrap(), rows);
    }
}
