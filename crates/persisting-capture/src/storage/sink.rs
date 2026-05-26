//! Capture sink: append trajectory records per session.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use serde_json::Value;

use super::markdown_pipeline::stamp_request_payload;
use super::record::{now_rfc3339, CaptureRecord};
use super::session::CaptureRoute;
use crate::capture_call::CaptureCall;
use crate::config::CaptureLevel;

pub trait CaptureSink: Send + Sync {
    /// Assign session-local `seq` on `record`, then persist. Mutates `record.seq` in place.
    fn append(
        &self,
        route: &CaptureRoute,
        agent_id: &str,
        record: &mut CaptureRecord,
    ) -> Result<()>;

    /// Next `seq` that [`Self::append`] would assign (does not increment).
    /// Returns `None` when the sink cannot predict seq (draft markdown preview unsupported).
    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        let _ = route;
        None
    }
}

/// Assigns monotonic `seq` per storage target and forwards records (RON encoding deferred to consumer).
pub struct CallbackSink {
    agent_id: String,
    next_seq: Mutex<HashMap<String, u64>>,
    callback: Box<dyn Fn(&CaptureRoute, &str, CaptureRecord) -> Result<()> + Send + Sync>,
}

impl CallbackSink {
    pub fn new<F>(agent_id: impl Into<String>, callback: F) -> Self
    where
        F: Fn(&CaptureRoute, &str, CaptureRecord) -> Result<()> + Send + Sync + 'static,
    {
        Self {
            agent_id: agent_id.into(),
            next_seq: Mutex::new(HashMap::new()),
            callback: Box::new(callback),
        }
    }
}

impl CaptureSink for CallbackSink {
    fn append(
        &self,
        route: &CaptureRoute,
        agent_id: &str,
        record: &mut CaptureRecord,
    ) -> Result<()> {
        let mut guard = self.next_seq.lock().unwrap();
        let seq = guard.entry(route.seq_key()).or_insert(0);
        record.seq = *seq;
        *seq += 1;
        drop(guard);
        record.session_id = Some(route.session_id.clone());
        record.subagent_id = route.subagent_id.clone();
        let aid = if agent_id.is_empty() {
            self.agent_id.as_str()
        } else {
            agent_id
        };
        (self.callback)(route, aid, record.clone())?;
        Ok(())
    }

    fn peek_next_seq(&self, route: &CaptureRoute) -> Option<u64> {
        Some(
            self.next_seq
                .lock()
                .unwrap()
                .get(&route.seq_key())
                .copied()
                .unwrap_or(0),
        )
    }
}

fn attach_call_context(rec: &mut CaptureRecord, call: &CaptureCall) {
    rec.trace_id = Some(call.trace_id.clone());
    rec.call_id = Some(call.call_id.clone());
}

pub fn llm_request_summary_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body_bytes: usize,
    protocol: &str,
    provider: &str,
    user_content: Option<String>,
    forward_to: Option<&str>,
    call: &CaptureCall,
    level: CaptureLevel,
    body_json: Option<&Value>,
) -> CaptureRecord {
    let mut payload = serde_json::json!({
        "model": model,
        "path": path,
        "body_bytes": body_bytes,
        "protocol": protocol,
        "provider": provider,
    });
    if level.includes_user_text() {
        if let Some(content) = user_content.filter(|s| !s.is_empty()) {
            payload["user_content"] = serde_json::Value::String(content);
        }
    }
    if let Some(fwd) = forward_to.filter(|s| !s.is_empty() && *s != model) {
        payload["forward_to"] = serde_json::Value::String(fwd.to_string());
    }
    if let Some(body) = body_json {
        stamp_request_payload(&mut payload, Some(body));
        if level.includes_full_body() {
            payload["body"] = body.clone();
        }
    }
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: "llm.request".to_string(),
        timestamp: Some(call.started_at.clone()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}

/// Full request body in payload — tests and fixtures only; production uses [`llm_request_summary_record`].
#[doc(hidden)]
pub fn llm_request_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    model: &str,
    path: &str,
    body: &serde_json::Value,
) -> CaptureRecord {
    CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: "llm.request".to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "model": model,
            "path": path,
            "body": body,
        }),
    }
}

pub fn llm_response_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    body: &serde_json::Value,
    streaming: bool,
    call: &CaptureCall,
) -> CaptureRecord {
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: if streaming {
            "llm.response.stream".to_string()
        } else {
            "llm.response".to_string()
        },
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({
            "status": status,
            "body": body,
        }),
    };
    attach_call_context(&mut rec, call);
    rec
}

pub fn llm_response_record_with_content(
    session_id: Option<String>,
    agent_id: Option<String>,
    status: u16,
    payload: &serde_json::Value,
    streaming: bool,
    assistant_content: Option<String>,
    call: &CaptureCall,
    level: CaptureLevel,
) -> CaptureRecord {
    let mut payload = payload.clone();
    payload["status"] = serde_json::json!(status);
    if level.includes_assistant_text() {
        if let Some(content) = assistant_content.filter(|s| !s.is_empty()) {
            payload["assistant_content"] = serde_json::Value::String(content);
        }
    }
    let kind = if streaming {
        "llm.response.stream"
    } else {
        "llm.response"
    };
    let mut rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: kind.to_string(),
        timestamp: Some(now_rfc3339()),
        session_id,
        agent_id,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload,
    };
    attach_call_context(&mut rec, call);
    rec
}
