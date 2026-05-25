//! Capture event types — canonical inputs to [`super::CaptureRuntime`].

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::Value;

use crate::capture_call::CaptureCall;
use crate::config::CaptureLevel;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::session_storage::CaptureRoute;
use crate::usage::StreamMetrics;

/// Per-request context shared across events for one LLM call.
#[derive(Clone)]
pub struct CaptureInvocation {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub call: CaptureCall,
    pub headers: HeaderMap,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: ProviderKind,
    pub protocol: ProtocolKind,
    pub storage: Arc<PathBuf>,
    pub debug_on: bool,
}

/// All capture-side effects are expressed as one of these events.
#[derive(Debug, Clone)]
pub enum CaptureEvent {
    /// User request observed at the proxy (before upstream).
    LlmRequest(LlmRequestCaptured),
    /// Final assistant response for one call (streaming or not).
    LlmResponseCompleted(LlmResponseCompleted),
    /// Streaming assistant draft — markdown view only (no Lance write).
    LlmResponseDraftUpdated(LlmResponseDraftUpdated),
    /// Client disconnected before stream finished (Lance only, no markdown).
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
    /// Final visible assistant text from the stream translator (preferred over re-parsing SSE).
    pub assistant_content: Option<String>,
}
