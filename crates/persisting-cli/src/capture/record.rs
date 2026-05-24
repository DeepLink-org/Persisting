//! Capture records for `capture import`.

pub use persisting_capture::record::{records_to_engine_lines, CaptureRecord};

#[derive(Debug, Clone)]
pub struct PendingRecord {
    pub sort_key: SortKey,
    pub source: String,
    pub kind: String,
    pub timestamp: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub parent_uuid: Option<String>,
    pub trace_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SortKey {
    pub ts_nanos: u64,
    pub file_order: u64,
    pub line_no: u64,
}

impl PendingRecord {
    pub fn into_capture(self, seq: u64) -> CaptureRecord {
        CaptureRecord {
            seq,
            source: self.source,
            kind: self.kind,
            timestamp: self.timestamp,
            session_id: self.session_id,
            agent_id: self.agent_id,
            parent_uuid: self.parent_uuid,
            trace_id: self.trace_id,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: self.payload,
        }
    }
}
