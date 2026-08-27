//! Shared Arrow row helpers for the canonical trajectory event log schema.

use std::sync::Arc;

use crate::{
    EventRecord, EventRow, TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_COLS,
    TRAJECTORY_EVENT_ID_COL, TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL,
    TRAJECTORY_PARENT_CALL_ID_COL, TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL,
    TRAJECTORY_SESSION_ID_COL, TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL,
    TRAJECTORY_TRACE_ID_COL, event_record_to_event_row, event_row_to_event_record,
};
use anyhow::{Context, Result};
use lance::deps::arrow_array::{
    Array, Int64Array, RecordBatch, StringArray, TimestampMillisecondArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, TimeUnit};

pub fn raw_event_arrow_schema() -> Arc<ArrowSchema> {
    Arc::new(ArrowSchema::new(vec![
        Field::new(TRAJECTORY_SEQ_COL, DataType::Int64, false),
        Field::new(TRAJECTORY_EVENT_ID_COL, DataType::Utf8, true),
        Field::new(
            TRAJECTORY_TIMESTAMP_COL,
            DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())),
            false,
        ),
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

pub fn event_rows_to_batch(schema: Arc<ArrowSchema>, rows: &[EventRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            )),
            Arc::new(opt_utf8(
                &rows.iter().map(|r| r.event_id.clone()).collect::<Vec<_>>(),
            )),
            Arc::new(
                TimestampMillisecondArray::from(
                    rows.iter().map(|row| row.timestamp).collect::<Vec<_>>(),
                )
                .with_timezone("UTC"),
            ),
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

pub(super) fn event_row_for_storage(
    storage_session_id: &str,
    storage_agent_id: &str,
    record: &EventRecord,
) -> Result<EventRow> {
    // Preserve the producer envelope in payload_json, but route and replay
    // through the physical coordinates selected by the capture caller.
    // Conflicting producer identity is evidence, not an append failure.
    let mut row = event_record_to_event_row(record)?;
    row.session_id = Some(storage_session_id.to_string());
    row.agent_id = Some(storage_agent_id.to_string());
    Ok(row)
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

fn timestamp_ms_at(batch: &RecordBatch, name: &str, row: usize) -> Result<i64> {
    let col_idx = batch
        .schema()
        .fields()
        .iter()
        .position(|field| field.name() == name)
        .ok_or_else(|| anyhow::anyhow!("batch missing column '{name}'"))?;
    let column = batch.column(col_idx);
    let array = column
        .as_any()
        .downcast_ref::<TimestampMillisecondArray>()
        .ok_or_else(|| anyhow::anyhow!("expected Timestamp(Millisecond, UTC) column {name}"))?;
    anyhow::ensure!(
        !array.is_null(row),
        "null canonical timestamp in column {name}"
    );
    Ok(array.value(row))
}

pub fn event_row_from_batch(batch: &RecordBatch, index: usize) -> Result<EventRow> {
    let payload_json = req_utf8_at(batch, TRAJECTORY_PAYLOAD_JSON_COL, index)?;
    let event_id = utf8_at(batch, TRAJECTORY_EVENT_ID_COL, index)?;
    Ok(EventRow {
        seq: seq_at(batch, index)?,
        event_id,
        timestamp: timestamp_ms_at(batch, TRAJECTORY_TIMESTAMP_COL, index)?,
        kind: req_utf8_at(batch, TRAJECTORY_KIND_COL, index)?,
        source: req_utf8_at(batch, TRAJECTORY_SOURCE_COL, index)?,
        agent_id: utf8_at(batch, TRAJECTORY_AGENT_ID_COL, index)?,
        session_id: utf8_at(batch, TRAJECTORY_SESSION_ID_COL, index)?,
        call_id: utf8_at(batch, TRAJECTORY_CALL_ID_COL, index)?,
        trace_id: utf8_at(batch, TRAJECTORY_TRACE_ID_COL, index)?,
        parent_call_id: utf8_at(batch, TRAJECTORY_PARENT_CALL_ID_COL, index)?,
        model: utf8_at(batch, TRAJECTORY_MODEL_COL, index)?,
        payload_json,
    })
}

pub fn event_records_from_batch(batch: &RecordBatch) -> Result<Vec<EventRecord>> {
    (0..batch.num_rows())
        .map(|i| {
            let row = event_row_from_batch(batch, i)?;
            event_row_to_event_record(&row)
        })
        .collect()
}

pub fn event_rows_from_batch(batch: &RecordBatch) -> Result<Vec<EventRow>> {
    (0..batch.num_rows())
        .map(|i| event_row_from_batch(batch, i))
        .collect()
}

pub fn schema_columns_note() -> String {
    TRAJECTORY_COLS.join(", ")
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
            batches.push(event_rows_to_batch(schema.clone(), chunk)?);
        }
        Ok(batches)
    }

    fn mk_row(seq: i64, session: &str, content: &str) -> EventRow {
        EventRow {
            seq,
            event_id: Some(format!("event-{seq}")),
            timestamp: 0,
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
    fn event_row_for_storage_stamps_physical_route_and_preserves_producer_seq() {
        let record = EventRecord {
            identity: crate::EventIdentity {
                event_id: Some("event-a".into()),
                timestamp_unix_ms: Some(0),
                ..Default::default()
            },
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("sess-a".into()),
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"a"}),
        };
        let row = event_row_for_storage("sess-a", "agent-a", &record).unwrap();
        assert_eq!(row.seq, 0);
        assert_eq!(row.session_id.as_deref(), Some("sess-a"));
        assert_eq!(row.agent_id.as_deref(), Some("agent-a"));
    }

    #[test]
    fn event_record_to_event_row_preserves_call_id() {
        let resp = EventRecord {
            identity: crate::EventIdentity {
                event_id: Some("event-response".into()),
                timestamp_unix_ms: Some(0),
                ..Default::default()
            },
            seq: 0,
            source: "test".into(),
            kind: "llm.response".into(),
            timestamp: None,
            session_id: Some("sess".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: Some("trace-a".into()),
            call_id: Some("call-a".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({
                "status": 200,
                "choices":[{"message":{"role":"assistant","content":"你好！"}}],
            }),
        };
        let row = event_row_for_storage("sess", "agent", &resp).unwrap();
        assert_eq!(row.call_id.as_deref(), Some("call-a"));
        assert_eq!(row.trace_id.as_deref(), Some("trace-a"));
        assert_eq!(row.seq, 0);

        let back = crate::event_row_to_event_record(&row).unwrap();
        assert_eq!(back.call_id.as_deref(), Some("call-a"));
        assert_eq!(back.seq, 0);
    }

    #[test]
    fn physical_routing_identity_wins_without_rewriting_payload_json() {
        let record = EventRecord {
            identity: crate::EventIdentity {
                timestamp_unix_ms: Some(0),
                ..Default::default()
            },
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("payload-session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"test"}),
        };
        let mut row = event_record_to_event_row(&record).unwrap();
        row.session_id = Some("physical-session".into());
        row.agent_id = Some("physical-agent".into());
        let replayed = crate::event_row_to_event_record(&row).unwrap();
        assert_eq!(replayed.session_id.as_deref(), Some("physical-session"));
        assert_eq!(replayed.agent_id.as_deref(), Some("physical-agent"));
        let preserved: EventRecord = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(preserved.session_id.as_deref(), Some("payload-session"));
        assert_eq!(preserved.agent_id.as_deref(), Some("agent"));
    }

    #[test]
    fn physical_seq_column_is_the_producer_storyline_seq() {
        let record = EventRecord {
            identity: crate::EventIdentity {
                event_id: Some("event-storyline-seq".into()),
                timestamp_unix_ms: Some(0),
                ..Default::default()
            },
            seq: 42,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("story".into()),
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"preserve me"}),
        };
        let row = event_record_to_event_row(&record).unwrap();
        assert_eq!(row.seq, 42);
        assert_eq!(crate::event_row_to_event_record(&row).unwrap().seq, 42);
    }

    #[test]
    fn record_batches_split_at_append_chunk_boundary() {
        let schema = raw_event_arrow_schema();
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
        let schema = raw_event_arrow_schema();
        let rows = vec![mk_row(0, "s", "hello"), mk_row(1, "s", "world")];
        let batch = event_rows_to_batch(schema, &rows).unwrap();
        let back = event_rows_from_batch(&batch).unwrap();
        assert_eq!(back, rows);
    }
}
