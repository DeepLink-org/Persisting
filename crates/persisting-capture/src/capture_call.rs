//! Per-LLM-call identity (LiteLLM-style call_id + trace_id).

use axum::http::HeaderMap;

use crate::record::now_rfc3339;
use crate::session_chain::{new_call_id, resolve_trace_id};

/// Identifiers for one LLM HTTP round-trip within a trajectory session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCall {
    pub call_id: String,
    pub trace_id: String,
    /// RFC3339 time when the HTTP request was accepted (timeline ordering).
    pub started_at: String,
}

impl CaptureCall {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let call_id = new_call_id();
        let trace_id = resolve_trace_id(headers, &call_id);
        Self {
            call_id,
            trace_id,
            started_at: now_rfc3339(),
        }
    }
}
