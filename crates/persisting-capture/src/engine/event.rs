use bytes::Bytes;
use serde_json::Value;

use crate::usage::StreamMetrics;

#[derive(Debug, Clone)]
pub enum CaptureEvent {
    LlmRequest(LlmRequestCaptured),
    LlmResponseCompleted(LlmResponseCompleted),
    LlmResponseDraftUpdated(LlmResponseDraftUpdated),
    LlmCallCancelled(LlmCallCancelled),
}

#[derive(Debug, Clone)]
pub struct LlmCallCancelled {
    pub status: u16,
    pub bytes_received: usize,
    pub streaming: bool,
}

#[derive(Debug, Clone)]
pub struct LlmResponseDraftUpdated {
    pub status: u16,
    pub assistant_content: String,
}

#[derive(Debug, Clone)]
pub struct LlmRequestCaptured {
    pub path: String,
    pub body_bytes: usize,
    pub user_content: Option<String>,
    pub body_json: Option<Value>,
    pub model_rewritten: bool,
}

#[derive(Debug, Clone)]
pub struct LlmResponseCompleted {
    pub status: u16,
    pub resp_bytes: Bytes,
    pub streaming: bool,
    pub stream_metrics: Option<StreamMetrics>,
    pub assistant_content: Option<String>,
}
