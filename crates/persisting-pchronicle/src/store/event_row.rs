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
    /// Canonical event time in Unix epoch milliseconds. The Arrow physical
    /// column is `Timestamp(Millisecond, UTC)`; the original textual timestamp
    /// remains losslessly available in `payload_json`.
    pub timestamp: i64,
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
        timestamp: canonical_timestamp_ms(rec)?,
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
        (
            "timestamp",
            canonical_timestamp_ms(&record)? == row.timestamp,
        ),
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

fn canonical_timestamp_ms(record: &EventRecord) -> Result<i64> {
    record
        .identity
        .timestamp_unix_ms
        .context("canonical event is missing timestamp_unix_ms")
        .and_then(|timestamp| {
            i64::try_from(timestamp).context("canonical event timestamp exceeds i64 milliseconds")
        })
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn identifier() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9._-]{1,24}").unwrap()
    }

    proptest! {
        #[test]
        fn event_rows_roundtrip_canonical_records(
            seq in 0i64..i64::MAX,
            timestamp in 0u64..=i64::MAX as u64,
            event_id in prop::option::of(identifier()),
            source in identifier(),
            kind in identifier(),
            agent_id in prop::option::of(identifier()),
            session_id in prop::option::of(identifier()),
            call_id in prop::option::of(identifier()),
            trace_id in prop::option::of(identifier()),
            parent_call_id in prop::option::of(identifier()),
            model in prop::option::of(identifier()),
        ) {
            let record = EventRecord {
                identity: crate::formats::events::EventIdentity {
                    event_id,
                    timestamp_unix_ms: Some(timestamp),
                    ..Default::default()
                },
                seq: seq as u64,
                source,
                kind,
                timestamp: None,
                session_id,
                agent_id,
                parent_uuid: None,
                trace_id,
                call_id,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id,
                payload: json!({"model": model, "nested": {"ok": true}}),
            };
            let row = event_record_to_event_row(&record).unwrap();
            let decoded = event_row_to_event_record(&row).unwrap();
            prop_assert_eq!(decoded, record);
        }

        #[test]
        fn event_rows_require_a_canonical_timestamp(
            seq in any::<u64>(),
            source in identifier(),
            kind in identifier(),
        ) {
            let record = EventRecord {
                identity: Default::default(),
                seq,
                source,
                kind,
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
                payload: json!({}),
            };
            prop_assert!(event_record_to_event_row(&record).is_err());
        }
    }
}
