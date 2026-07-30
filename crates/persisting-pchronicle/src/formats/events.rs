//! `events` — Capture HTTP-first SoT, **Lance-only** as a first-class format.
//!
//! Production storage is `{run}/events.lance/` (Lance dataset). pChronicle does
//! **not** treat JSON / JSONL as a supported events wire format.
//!
//! What this module provides:
//! - [`EventRecord`] / [`EventsDocument`]: in-memory shape aligned with CaptureRecord
//! - [`events_to_storyline`](crate::convert::events_to_storyline) / storyline→events
//!   (programmatic, after you already loaded rows from Lance)
//! - [`export_events_jsonl`] / [`export_events_json_pretty`]: **debug export only**
//!
//! Use `persisting traj` (or similar) to extract Lance → JSON for inspection.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

/// One capture event — field-compatible with `persisting_capture::record::CaptureRecord`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub seq: u64,
    pub source: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_call_id: Option<String>,
    pub payload: Value,
}

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
        let session_id = events.iter().find_map(|e| e.session_id.clone());
        let agent_id = events.iter().find_map(|e| e.agent_id.clone());
        Self {
            format: Self::FORMAT_NAME.into(),
            session_id,
            agent_id,
            events,
        }
    }
}

/// Error message shared by string-based convert APIs.
pub fn events_lance_only_message() -> &'static str {
    "events is Lance-only (events.lance); JSON/JSONL is not a supported wire format. \
     Extract with traj/export tools, or call events_to_storyline / storyline_to_events \
     on in-memory EventRecord batches after loading Lance."
}

pub fn events_lance_only_error() -> Error {
    Error::Other(events_lance_only_message().into())
}

/// Debug/export helper: serialize records as JSONL. **Not** a chronicle format.
pub fn export_events_jsonl(events: &[EventRecord]) -> Result<String> {
    let mut out = String::new();
    for event in events {
        out.push_str(&serde_json::to_string(event)?);
        out.push('\n');
    }
    Ok(out)
}

/// Debug/export helper: pretty JSON document. **Not** a chronicle format.
pub fn export_events_json_pretty(doc: &EventsDocument) -> Result<String> {
    Ok(serde_json::to_string_pretty(doc)?)
}

/// Test/fixture helper: parse JSONL into memory. **Not** part of the public format API surface
/// for `into_storyline` / `convert`.
#[cfg(test)]
pub fn parse_events_jsonl_for_test(input: &str) -> Result<EventsDocument> {
    let mut events = Vec::new();
    for (idx, line) in input.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event = serde_json::from_str::<EventRecord>(line).map_err(|e| {
            Error::Other(format!("events jsonl line {}: {e}", idx + 1))
        })?;
        events.push(event);
    }
    Ok(EventsDocument::new(events))
}
