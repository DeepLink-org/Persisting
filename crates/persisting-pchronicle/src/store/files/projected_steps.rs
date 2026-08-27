//! Shared projected step row batching for ATIF and ACTF streaming queries.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use lance::deps::arrow_array::{
    ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchOptions, StringArray,
};
use lance::deps::arrow_schema::SchemaRef;
use tokio::sync::mpsc::Sender;

use super::{FileState, FileTrajectoryRuntime, SOURCE_FILE_COLUMN};
use crate::model::StorylineTimestamp;
use crate::store::storyline::rows::timestamp_array;

pub(crate) struct ProjectedStepRow {
    pub document_id: String,
    pub run_id: Option<String>,
    pub session_id: String,
    pub step_id: i64,
    pub kind: Option<String>,
    pub effective_kind: String,
    pub timestamp: Option<String>,
    pub source: String,
    pub message_json: String,
    pub reasoning_content: Option<String>,
    pub reasoning_effort_json: Option<String>,
    pub metrics_json: Option<String>,
    pub model_name: Option<String>,
    pub llm_call_count: Option<i64>,
    pub is_copied_context: Option<bool>,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub had_observation: bool,
    pub extra_json: Option<String>,
}

/// Renders JSON captured as a [`serde_json::value::RawValue`] in the same
/// canonical form the fully materialized path emits.
///
/// Raw capture keeps the source bytes verbatim, so without this the emitted
/// column would echo the input file's indentation and key order and identical
/// data stored in differently formatted files would compare unequal.
pub(crate) fn canonical_json_text(raw: &serde_json::value::RawValue) -> String {
    match serde_json::from_str::<serde_json::Value>(raw.get()) {
        Ok(value) => value.to_string(),
        // The text came from a completed parse, so this is unreachable; fall
        // back to the raw bytes rather than failing the whole scan.
        Err(_) => raw.get().to_string(),
    }
}

pub(crate) fn projected_timing_from_metrics(
    metrics_json: Option<&str>,
) -> (Option<i64>, Option<i64>) {
    let Some(metrics_json) = metrics_json else {
        return (None, None);
    };
    #[derive(serde::Deserialize)]
    struct MetricsProjection {
        latency_ms: Option<serde_json::Number>,
        elapsed_ms: Option<serde_json::Number>,
        duration_ms: Option<serde_json::Number>,
        ttft_ms: Option<serde_json::Number>,
    }
    let Ok(metrics) = serde_json::from_str::<MetricsProjection>(metrics_json) else {
        return (None, None);
    };
    let parse = |value: Option<serde_json::Number>| {
        value.and_then(|number| {
            number
                .as_i64()
                .or_else(|| number.as_f64().map(|value| value as i64))
        })
    };
    let latency_ms = parse(metrics.latency_ms)
        .or_else(|| parse(metrics.elapsed_ms))
        .or_else(|| parse(metrics.duration_ms));
    let ttft_ms = parse(metrics.ttft_ms);
    (latency_ms, ttft_ms)
}

pub(crate) fn projected_timing_from_actf_metrics(metrics_json: Option<&str>) -> Option<i64> {
    let metrics_json = metrics_json?;
    #[derive(serde::Deserialize)]
    struct ActfMetricProjection {
        llm_infer_ms: Option<serde_json::Value>,
    }
    let Ok(metrics) = serde_json::from_str::<ActfMetricProjection>(metrics_json) else {
        return None;
    };
    metrics.llm_infer_ms.and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

pub(crate) fn emit_projected_step_batch(
    rows: &mut Vec<ProjectedStepRow>,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<bool> {
    if rows.is_empty() {
        return Ok(true);
    }
    let batch = projected_step_rows_to_batch(rows, file.file.relative_path(), schema.clone())?;
    rows.clear();
    runtime
        .metrics
        .inner
        .projected_arrow_bytes
        .fetch_add(batch.get_array_memory_size() as u64, Ordering::Relaxed);
    Ok(tx.blocking_send(Ok(batch)).is_ok())
}

pub(crate) fn projected_step_rows_to_batch(
    rows: &[ProjectedStepRow],
    relative_path: &str,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let timestamps = rows
        .iter()
        .map(|row| {
            row.timestamp
                .as_deref()
                .map(StorylineTimestamp::from_rfc3339)
                .transpose()
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let timestamp_sources = timestamps
        .iter()
        .map(|timestamp| {
            timestamp
                .as_ref()
                .map(|timestamp| serde_json::to_string(timestamp.source_value()))
                .transpose()
                .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut columns = Vec::<ArrayRef>::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let column: ArrayRef = match field.name().as_str() {
            "document_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.document_id.as_str()),
            )),
            "session_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.session_id.as_str()),
            )),
            "run_id" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.run_id.as_deref()),
            )),
            "step_id" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.step_id).collect::<Vec<_>>(),
            )),
            "kind" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.kind.as_deref()),
            )),
            "effective_kind" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.effective_kind.as_str()),
            )),
            "timestamp" => Arc::new(timestamp_array(timestamps.iter().map(Option::as_ref))),
            "timestamp_source_json" => Arc::new(StringArray::from_iter(
                timestamp_sources.iter().map(Option::as_deref),
            )),
            "timestamp_rfc3339" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.timestamp.as_deref()),
            )),
            "source" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.source.as_str()),
            )),
            "message_json" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.message_json.as_str()),
            )),
            "reasoning_content" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.reasoning_content.as_deref()),
            )),
            "reasoning_effort_json" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.reasoning_effort_json.as_deref()),
            )),
            "metrics_json" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.metrics_json.as_deref()),
            )),
            "model_name" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.model_name.as_deref()),
            )),
            "llm_call_count" => Arc::new(Int64Array::from(
                rows.iter()
                    .map(|row| row.llm_call_count)
                    .collect::<Vec<_>>(),
            )),
            "is_copied_context" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.is_copied_context)
                    .collect::<Vec<_>>(),
            )),
            "latency_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.latency_ms).collect::<Vec<_>>(),
            )),
            "ttft_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.ttft_ms).collect::<Vec<_>>(),
            )),
            "had_observation" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.had_observation)
                    .collect::<Vec<_>>(),
            )),
            "extra_json" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.extra_json.as_deref()),
            )),
            SOURCE_FILE_COLUMN => Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                relative_path,
                rows.len(),
            ))),
            name => anyhow::bail!("unsupported projected steps column '{name}'"),
        };
        columns.push(column);
    }
    let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
    RecordBatch::try_new_with_options(schema, columns, &options)
        .context("build projected steps batch")
}
