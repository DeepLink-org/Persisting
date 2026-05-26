//! One LLM HTTP round-trip within a story.

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::session_chain::{new_call_id, resolve_trace_id};
use crate::storage::record::now_rfc3339;

use super::ids::CallId;

/// Identifiers for one proxied LLM HTTP call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub call_id: String,
    pub trace_id: String,
    /// RFC3339 time when the HTTP request was accepted.
    pub started_at: String,
}

impl Call {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let call_id = new_call_id();
        let trace_id = resolve_trace_id(headers, &call_id);
        Self {
            call_id,
            trace_id,
            started_at: now_rfc3339(),
        }
    }

    pub fn id(&self) -> CallId {
        CallId::new(&self.call_id)
    }
}
