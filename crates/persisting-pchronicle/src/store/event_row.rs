//! Lance event row ↔ [`EventRecord`].

use anyhow::{Context, Result};

use crate::formats::events::EventRecord;

/// One row in the Lance event log (canonical trajectory store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: i64,
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

pub fn event_record_to_event_row(rec: &EventRecord, seq: i64) -> Result<EventRow> {
    Ok(EventRow {
        seq,
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
    let mut rec: EventRecord =
        serde_json::from_str(&row.payload_json).context("decode EventRecord JSON")?;
    rec.seq = u64::try_from(row.seq).context("seq out of range for EventRecord")?;
    Ok(rec)
}

pub fn event_row_to_replay_json(row: &EventRow) -> Result<String> {
    let rec = event_row_to_event_record(row)?;
    serde_json::to_string(&rec).context("encode replay JSON")
}
