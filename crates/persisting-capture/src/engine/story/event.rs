//! Capture events on a call: request → draft* → complete | cancel.

use bytes::Bytes;
use serde_json::Value;

use crate::usage::StreamMetrics;

/// Something that happened on one LLM call during capture.
#[derive(Debug, Clone)]
pub enum Event {
    Request(RequestEvent),
    ResponseDraft(DraftEvent),
    ResponseComplete(CompleteEvent),
    Cancelled(CancelEvent),
}

#[derive(Debug, Clone)]
pub struct CancelEvent {
    pub status: u16,
    pub bytes_received: usize,
    pub streaming: bool,
}

#[derive(Debug, Clone)]
pub struct DraftEvent {
    pub status: u16,
    pub assistant_content: String,
}

#[derive(Debug, Clone)]
pub struct RequestEvent {
    pub path: String,
    pub body_bytes: usize,
    pub user_content: Option<String>,
    pub body_json: Option<Value>,
    pub model_rewritten: bool,
}

#[derive(Debug, Clone)]
pub struct CompleteEvent {
    pub status: u16,
    pub resp_bytes: Bytes,
    pub streaming: bool,
    pub stream_metrics: Option<StreamMetrics>,
    pub assistant_content: Option<String>,
}
