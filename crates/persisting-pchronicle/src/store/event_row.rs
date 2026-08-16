//! Lance event row ↔ [`EventRecord`].

use anyhow::{Context, Result};

use crate::formats::events::EventRecord;

/// One row in the Lance event log (canonical trajectory store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// Producer-defined Storyline sequence. Replay uses physical append order.
    pub seq: i64,
    /// Opaque business identity. Duplicates are valid appended facts.
    pub event_id: Option<String>,
    pub timestamp: Option<String>,
    pub kind: String,
    pub source: String,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub parent_call_id: Option<String>,
    pub model: Option<String>,
    /// Full [`EventRecord`] JSON; indexed columns are denormalized for filtering.
    pub payload_json: String,
}

fn index_model(rec: &EventRecord) -> Option<String> {
    rec.payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

pub fn event_record_to_event_row(rec: &EventRecord) -> Result<EventRow> {
    Ok(EventRow {
        seq: i64::try_from(rec.seq).context("EventRecord seq exceeds i64")?,
        event_id: rec.identity.event_id.clone(),
        timestamp: rec.timestamp.clone(),
        kind: rec.kind.clone(),
        source: rec.source.clone(),
        agent_id: rec.agent_id.clone(),
        session_id: rec.session_id.clone(),
        call_id: rec.call_id.clone(),
        trace_id: rec.trace_id.clone(),
        parent_call_id: rec.parent_call_id.clone(),
        model: index_model(rec),
        payload_json: serde_json::to_string(rec).context("encode EventRecord JSON")?,
    })
}

pub fn event_row_to_event_record(row: &EventRow) -> Result<EventRecord> {
    let mut record: EventRecord =
        serde_json::from_str(&row.payload_json).context("decode EventRecord JSON")?;
    record.validate().map_err(anyhow::Error::from)?;
    let seq = i64::try_from(record.seq).context("EventRecord seq exceeds i64")?;
    for (field, matches) in [
        ("seq", seq == row.seq),
        ("event_id", record.identity.event_id == row.event_id),
        ("timestamp", record.timestamp == row.timestamp),
        ("kind", record.kind == row.kind),
        ("source", record.source == row.source),
        ("call_id", record.call_id == row.call_id),
        ("trace_id", record.trace_id == row.trace_id),
        (
            "parent_call_id",
            record.parent_call_id == row.parent_call_id,
        ),
        ("model", index_model(&record) == row.model),
    ] {
        anyhow::ensure!(
            matches,
            "{field} mismatch between physical column and payload_json"
        );
    }
    // The append caller's physical routing coordinates are authoritative at
    // replay time. payload_json still contains the producer's original claim,
    // so admission remains lossless without adding conflict columns or indexes.
    record.agent_id.clone_from(&row.agent_id);
    record.session_id.clone_from(&row.session_id);
    Ok(record)
}
