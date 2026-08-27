//! `events` — Capture HTTP-first SoT, with Lance as the canonical query format.
//!
//! Production storage is `{run}/events.lance/` (Lance dataset). pChronicle does
//! The pChronicle control path may additionally archive complete EventRecords
//! as JSON in a warehouse prefix; conversion/query APIs below still operate on
//! Lance rows.
//!
//! What this module provides:
//! - [`EventRecord`] / [`EventsDocument`]: in-memory shape aligned with EventRecord
//! - [`events_to_storyline`] / [`storyline_to_events`] (programmatic, after you
//!   already loaded rows from Lance)
//! - [`export_events_jsonl`]: test/debug export from canonical Lance rows
//!
//! Use the pChronicle APIs to extract Lance events for inspection.

use serde::{Deserialize, Serialize};

use crate::Result;
pub use persisting_events::{EventIdentity, EventRecord};

mod convert;
#[cfg(feature = "lance-store")]
pub(crate) use convert::event_storyline_key;
pub use convert::{events_to_storyline, project_event_records, storyline_to_events};

/// In-memory batch of events (not a file format).
///
/// Built after reading Lance rows or in unit tests. Do not treat serialized
/// [`EventsDocument`] JSON as a supported interchange format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsDocument {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub events: Vec<EventRecord>,
}

impl EventsDocument {
    pub const FORMAT_NAME: &'static str = "events";

    pub fn new(events: Vec<EventRecord>) -> Self {
        let session_id = events.iter().rev().find_map(|e| e.session_id.clone());
        let agent_id = events.iter().rev().find_map(|e| e.agent_id.clone());
        Self {
            format: Self::FORMAT_NAME.into(),
            session_id,
            agent_id,
            events,
        }
    }
}

pub trait ChronicleEventRecordExt {
    /// Decode the canonical typed semantics attached to an `llm.request` event.
    /// Wire-only and legacy events legitimately return `None`.
    fn llm_request_payload(&self) -> Result<Option<super::llm::LlmRequestEventPayload>>;

    /// Decode the canonical typed semantics attached to an `llm.response` event.
    fn llm_response_payload(&self) -> Result<Option<super::llm::LlmResponseEventPayload>>;
}

impl ChronicleEventRecordExt for EventRecord {
    fn llm_request_payload(&self) -> Result<Option<super::llm::LlmRequestEventPayload>> {
        let Some(payload) = self.payload.get("llm_request") else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_value(payload.clone())?))
    }

    fn llm_response_payload(&self) -> Result<Option<super::llm::LlmResponseEventPayload>> {
        let Some(payload) = self.payload.get("llm_response") else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_value(payload.clone())?))
    }
}

/// Error message shared by string-based convert APIs.
#[cfg(all(test, feature = "lance-store"))]
pub fn events_lance_only_message() -> &'static str {
    "events is Lance-only (events.lance); JSON/JSONL is not a supported wire format. \
     Extract with traj/export tools, or call events_to_storyline / storyline_to_events \
     on in-memory EventRecord batches after loading Lance."
}

/// Debug/export helper: serialize records as JSONL. **Not** a chronicle format.
#[cfg(all(test, feature = "lance-store"))]
pub fn export_events_jsonl(events: &[EventRecord]) -> Result<String> {
    let mut out = String::new();
    for event in events {
        out.push_str(&serde_json::to_string(event)?);
        out.push('\n');
    }
    Ok(out)
}

/// Debug/export helper: pretty JSON document. **Not** a chronicle format.
/// Test/fixture helper: parse JSONL into memory. **Not** part of the public format API surface
/// for `into_storyline` / `convert`.
#[cfg(all(test, feature = "lance-store"))]
pub fn parse_events_jsonl_for_test(input: &str) -> Result<EventsDocument> {
    let mut events = Vec::new();
    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = serde_json::from_str::<EventRecord>(line)?;
        events.push(event);
    }
    Ok(EventsDocument::new(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn event_wire_has_no_schema_version() {
        let value = serde_json::json!({
            "seq": 0,
            "source": "capture",
            "kind": "llm.request",
            "payload": {}
        });
        let record = serde_json::from_value::<EventRecord>(value).unwrap();
        assert!(
            serde_json::to_value(record)
                .unwrap()
                .get("schema_version")
                .is_none()
        );
    }

    #[test]
    fn runtime_identity_is_flattened() {
        let mut record = EventRecord {
            identity: EventIdentity::default(),
            seq: 0,
            source: "capture".into(),
            kind: "llm.request".into(),
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
            payload: Value::Object(Default::default()),
        };
        record.identity.event_id = Some("event-1".into());
        record.identity.run_id = Some("run-1".into());
        let encoded = serde_json::to_value(record).unwrap();
        assert_eq!(encoded["event_id"], "event-1");
        assert_eq!(encoded["run_id"], "run-1");
        assert!(encoded.get("identity").is_none());
    }

    #[test]
    fn typed_llm_payload_decodes_from_event_envelope() {
        let record = EventRecord {
            identity: EventIdentity::default(),
            seq: 1,
            source: "gateway".into(),
            kind: "llm.request".into(),
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
            payload: serde_json::json!({
                "llm_request": {
                    "input_format":"chat_completions",
                    "request": {
                        "model":"m",
                        "system":[],
                        "messages":[],
                        "generation":{},
                        "stream":false
                    }
                }
            }),
        };
        let payload = record.llm_request_payload().unwrap().unwrap();
        assert_eq!(payload.request.model.as_deref(), Some("m"));
        assert_eq!(
            payload.input_format,
            super::super::llm::LlmProtocol::ChatCompletions
        );
    }
}
