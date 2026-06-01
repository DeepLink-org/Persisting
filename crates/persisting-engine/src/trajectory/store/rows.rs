//! Shared Arrow row helpers for trajectory event log backends (schema v1).

use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema as ArrowSchema};
use persisting_capture::event_row::{engine_line_to_event_row, event_row_to_replay_json, EventRow};

use crate::trajectory::{
    TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL,
    TRAJECTORY_PARENT_CALL_ID_COL, TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL,
    TRAJECTORY_SESSION_ID_COL, TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL,
    TRAJECTORY_TRACE_ID_COL, TRAJECTORY_V1_COLS,
};

pub fn trajectory_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        Field::new(TRAJECTORY_SEQ_COL, DataType::Int64, false),
        Field::new(TRAJECTORY_TIMESTAMP_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_KIND_COL, DataType::Utf8, false),
        Field::new(TRAJECTORY_SOURCE_COL, DataType::Utf8, false),
        Field::new(TRAJECTORY_AGENT_ID_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_SESSION_ID_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_CALL_ID_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_TRACE_ID_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_PARENT_CALL_ID_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_MODEL_COL, DataType::Utf8, true),
        Field::new(TRAJECTORY_PAYLOAD_JSON_COL, DataType::Utf8, false),
    ]))
}

fn opt_utf8(values: &[Option<String>]) -> StringArray {
    StringArray::from(values.iter().map(|v| v.as_deref()).collect::<Vec<_>>())
}

fn req_utf8(values: &[String]) -> StringArray {
    StringArray::from(values.iter().map(|s| s.as_str()).collect::<Vec<_>>())
}

pub fn record_batch_from_rows(schema: Arc<ArrowSchema>, rows: &[EventRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.timestamp.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(req_utf8(
                &rows.iter().map(|r| r.kind.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(req_utf8(
                &rows.iter().map(|r| r.source.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.agent_id.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows
                    .iter()
                    .map(|r| r.session_id.clone())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.call_id.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.trace_id.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows
                    .iter()
                    .map(|r| r.parent_call_id.clone())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.model.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(req_utf8(
                &rows
                    .iter()
                    .map(|r| r.payload_json.clone())
                    .collect::<Vec<_>>(),
            )),
        ],
    )
    .context("build trajectory RecordBatch")
}

pub fn rows_for_lines(
    storage_session_id: &str,
    mut start_seq: i64,
    lines: &[String],
) -> Result<Vec<EventRow>> {
    let mut rows = Vec::with_capacity(lines.len());
    for line in lines {
        let mut row = engine_line_to_event_row(line, start_seq)?;
        row.session_id = Some(storage_session_id.to_string());
        rows.push(row);
        start_seq += 1;
    }
    Ok(rows)
}

pub fn utf8_at(batch: &RecordBatch, name: &str, row: usize) -> Result<Option<String>> {
    let col_idx = batch
        .schema()
        .fields()
        .iter()
        .position(|f| f.name() == name)
        .ok_or_else(|| anyhow::anyhow!("batch missing column '{name}'"))?;
    let col = batch.column(col_idx);
    let Some(a) = col.as_any().downcast_ref::<StringArray>() else {
        anyhow::bail!("expected Utf8 column {name}");
    };
    if a.is_null(row) {
        Ok(None)
    } else {
        Ok(Some(a.value(row).to_string()))
    }
}

pub fn req_utf8_at(batch: &RecordBatch, name: &str, row: usize) -> Result<String> {
    utf8_at(batch, name, row)?.ok_or_else(|| anyhow::anyhow!("null required column {name}"))
}

pub fn seq_at(batch: &RecordBatch, row: usize) -> Result<i64> {
    let col_idx = batch
        .schema()
        .fields()
        .iter()
        .position(|f| f.name() == TRAJECTORY_SEQ_COL)
        .ok_or_else(|| anyhow::anyhow!("batch missing {}", TRAJECTORY_SEQ_COL))?;
    let col = batch.column(col_idx);
    let Some(a) = col.as_any().downcast_ref::<Int64Array>() else {
        anyhow::bail!("expected Int64 column {}", TRAJECTORY_SEQ_COL);
    };
    Ok(a.value(row))
}

pub fn row_from_batch(batch: &RecordBatch, index: usize) -> Result<EventRow> {
    Ok(EventRow {
        seq: seq_at(batch, index)?,
        timestamp: utf8_at(batch, TRAJECTORY_TIMESTAMP_COL, index)?,
        kind: req_utf8_at(batch, TRAJECTORY_KIND_COL, index)?,
        source: req_utf8_at(batch, TRAJECTORY_SOURCE_COL, index)?,
        agent_id: utf8_at(batch, TRAJECTORY_AGENT_ID_COL, index)?,
        session_id: utf8_at(batch, TRAJECTORY_SESSION_ID_COL, index)?,
        call_id: utf8_at(batch, TRAJECTORY_CALL_ID_COL, index)?,
        trace_id: utf8_at(batch, TRAJECTORY_TRACE_ID_COL, index)?,
        parent_call_id: utf8_at(batch, TRAJECTORY_PARENT_CALL_ID_COL, index)?,
        model: utf8_at(batch, TRAJECTORY_MODEL_COL, index)?,
        payload_json: req_utf8_at(batch, TRAJECTORY_PAYLOAD_JSON_COL, index)?,
    })
}

pub fn replay_records_from_batch(batch: &RecordBatch) -> Result<Vec<String>> {
    (0..batch.num_rows())
        .map(|i| {
            let row = row_from_batch(batch, i)?;
            event_row_to_replay_json(&row)
        })
        .collect()
}

pub fn rows_from_batch(batch: &RecordBatch) -> Result<Vec<EventRow>> {
    (0..batch.num_rows())
        .map(|i| row_from_batch(batch, i))
        .collect()
}

pub fn reassign_global_seq(rows: &mut [EventRow]) {
    for (seq, row) in rows.iter_mut().enumerate() {
        row.seq = seq as i64;
    }
}

pub fn schema_columns_note() -> String {
    TRAJECTORY_V1_COLS.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPEND_CHUNK_ROWS: usize = 8192;

    fn record_batches_for_rows(
        schema: Arc<ArrowSchema>,
        rows: &[EventRow],
    ) -> Result<Vec<RecordBatch>> {
        let mut batches = Vec::new();
        for chunk in rows.chunks(APPEND_CHUNK_ROWS) {
            batches.push(record_batch_from_rows(schema.clone(), chunk)?);
        }
        Ok(batches)
    }

    fn mk_row(seq: i64, session: &str, content: &str) -> EventRow {
        EventRow {
            seq,
            timestamp: None,
            kind: "note".into(),
            source: "test".into(),
            agent_id: Some("agent".into()),
            session_id: Some(session.into()),
            call_id: None,
            trace_id: None,
            parent_call_id: None,
            model: None,
            payload_json: format!(r#"{{"content":"{content}"}}"#),
        }
    }

    #[test]
    fn reassign_global_seq_renumbers_contiguously() {
        let mut rows = vec![
            mk_row(99, "a", "x"),
            mk_row(5, "b", "y"),
            mk_row(42, "a", "z"),
        ];
        reassign_global_seq(&mut rows);
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn rows_for_lines_stamps_session_and_monotonic_seq() {
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::record::CaptureRecord;

        let line = record_to_engine_line(&CaptureRecord {
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
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
            payload: serde_json::json!({"content":"a"}),
        })
        .unwrap();
        let rows = rows_for_lines("sess-a", 10, std::slice::from_ref(&line)).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 10);
        assert_eq!(rows[0].session_id.as_deref(), Some("sess-a"));
    }

    #[test]
    fn record_batches_split_at_append_chunk_boundary() {
        let schema = trajectory_schema();
        let rows: Vec<_> = (0..APPEND_CHUNK_ROWS + 1)
            .map(|i| mk_row(i as i64, "s", &format!("row-{i}")))
            .collect();
        let batches = record_batches_for_rows(schema, &rows).unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].num_rows(), APPEND_CHUNK_ROWS);
        assert_eq!(batches[1].num_rows(), 1);
    }

    #[test]
    fn roundtrip_batch_rows_preserves_payload() {
        let schema = trajectory_schema();
        let rows = vec![mk_row(0, "s", "hello"), mk_row(1, "s", "world")];
        let batch = record_batch_from_rows(schema, &rows).unwrap();
        let back = rows_from_batch(&batch).unwrap();
        assert_eq!(back, rows);
    }
}
