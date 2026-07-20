use crate::dialogue::InferenceEndpoint;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldSink {
    TopLevel,
    Extensions,
    Capture,
}

#[derive(Debug, Clone)]
pub struct FieldPatch {
    pub sink: FieldSink,
    pub value: Value,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureMeta {
    pub finish_reason: Option<String>,
    pub usage: Option<Value>,
    pub segment_kind: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CaptureEvent {
    pub call_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub step_id: u64,
    pub turn: u64,
    pub endpoint: InferenceEndpoint,
    pub request_path: String,
    pub model: String,
    pub request: Value,
    pub request_headers: BTreeMap<String, String>,
    pub response_raw: Value,
    pub response_text: Option<String>,
    pub stream: bool,
    pub status_code: u16,
    pub completed_at: DateTime<Utc>,
    pub metadata: BTreeMap<String, Value>,
    pub field_patches: BTreeMap<String, FieldPatch>,
    pub capture_meta: CaptureMeta,
    pub user_seq: u64,
    pub assistant_seq: u64,
}
