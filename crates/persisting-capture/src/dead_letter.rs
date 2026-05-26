//! Failed capture events — append-only JSONL for recovery (`{storage}/.capture/dead_letter.jsonl`).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capture_call::CaptureCall;
use crate::config::CaptureLevel;
use crate::engine::{
    CaptureEvent, CaptureInvocation, LlmCallCancelled, LlmRequestCaptured, LlmResponseCompleted,
    LlmResponseDraftUpdated,
};
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::session_storage::CaptureRoute;
use crate::usage::StreamMetrics;

const DEAD_LETTER_FILENAME: &str = "dead_letter.jsonl";

/// Serializable invocation snapshot for dead-letter replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadLetterInvocation {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub call: CaptureCall,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    pub provider: String,
    pub protocol: String,
}

/// Serializable capture event (wire format for JSONL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializableCaptureEvent {
    LlmRequest {
        path: String,
        body_bytes: usize,
        user_content: Option<String>,
        body_json: Option<Value>,
        model_rewritten: bool,
    },
    LlmResponseCompleted {
        status: u16,
        resp_payload: RespPayload,
        streaming: bool,
        stream_metrics: Option<StreamMetrics>,
        assistant_content: Option<String>,
    },
    LlmResponseDraftUpdated {
        status: u16,
        assistant_content: String,
    },
    LlmCallCancelled {
        status: u16,
        bytes_received: usize,
        streaming: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RespPayload {
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeadLetterEntry {
    pub timestamp: String,
    pub invocation: DeadLetterInvocation,
    pub event: SerializableCaptureEvent,
    pub error: String,
    /// Set when prepare produced a record but session dispatch failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_record_json: Option<String>,
}

pub fn dead_letter_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join(DEAD_LETTER_FILENAME)
}

pub fn append_dead_letter(
    storage: &Path,
    inv: &CaptureInvocation,
    event: &CaptureEvent,
    error: &str,
    prepared_record_json: Option<String>,
) -> Result<()> {
    let entry = DeadLetterEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        invocation: DeadLetterInvocation::from_invocation(inv),
        event: SerializableCaptureEvent::from_event(event),
        error: error.to_string(),
        prepared_record_json,
    };
    let path = dead_letter_path(storage);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create .capture for dead letter")?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open dead letter {}", path.display()))?;
    let line = serde_json::to_string(&entry).context("serialize dead letter entry")?;
    writeln!(file, "{line}").context("append dead letter")?;
    Ok(())
}

pub fn read_dead_letter_entries(storage: &Path) -> Result<Vec<DeadLetterEntry>> {
    let path = dead_letter_path(storage);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read dead letter line {i}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(trimmed).with_context(|| format!("parse dead letter line {i}"))?,
        );
    }
    Ok(out)
}

impl DeadLetterInvocation {
    pub fn from_invocation(inv: &CaptureInvocation) -> Self {
        Self {
            route: inv.route.clone(),
            agent_id: inv.agent_id.clone(),
            call: inv.call.clone(),
            level: inv.level,
            client_model: inv.client_model.clone(),
            upstream_model: inv.upstream_model.clone(),
            provider: inv.provider.as_str().to_string(),
            protocol: inv.protocol.as_str().to_string(),
        }
    }

    pub fn to_invocation(&self, storage: Arc<std::path::PathBuf>) -> CaptureInvocation {
        CaptureInvocation {
            route: self.route.clone(),
            agent_id: self.agent_id.clone(),
            call: self.call.clone(),
            headers: axum::http::HeaderMap::new(),
            level: self.level,
            client_model: self.client_model.clone(),
            upstream_model: self.upstream_model.clone(),
            provider: ProviderKind::parse(&self.provider),
            protocol: ProtocolKind::parse(&self.protocol),
            storage,
            debug_on: false,
        }
    }
}

impl SerializableCaptureEvent {
    pub fn from_event(event: &CaptureEvent) -> Self {
        match event {
            CaptureEvent::LlmRequest(e) => Self::LlmRequest {
                path: e.path.clone(),
                body_bytes: e.body_bytes,
                user_content: e.user_content.clone(),
                body_json: e.body_json.clone(),
                model_rewritten: e.model_rewritten,
            },
            CaptureEvent::LlmResponseCompleted(e) => Self::LlmResponseCompleted {
                status: e.status,
                resp_payload: resp_to_payload(&e.resp_bytes),
                streaming: e.streaming,
                stream_metrics: e.stream_metrics.clone(),
                assistant_content: e.assistant_content.clone(),
            },
            CaptureEvent::LlmResponseDraftUpdated(e) => Self::LlmResponseDraftUpdated {
                status: e.status,
                assistant_content: e.assistant_content.clone(),
            },
            CaptureEvent::LlmCallCancelled(e) => Self::LlmCallCancelled {
                status: e.status,
                bytes_received: e.bytes_received,
                streaming: e.streaming,
            },
        }
    }

    pub fn to_event(&self) -> CaptureEvent {
        match self {
            Self::LlmRequest {
                path,
                body_bytes,
                user_content,
                body_json,
                model_rewritten,
            } => CaptureEvent::LlmRequest(LlmRequestCaptured {
                path: path.clone(),
                body_bytes: *body_bytes,
                user_content: user_content.clone(),
                body_json: body_json.clone(),
                model_rewritten: *model_rewritten,
            }),
            Self::LlmResponseCompleted {
                status,
                resp_payload,
                streaming,
                stream_metrics,
                assistant_content,
            } => CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                status: *status,
                resp_bytes: payload_to_bytes(resp_payload),
                streaming: *streaming,
                stream_metrics: stream_metrics.clone(),
                assistant_content: assistant_content.clone(),
            }),
            Self::LlmResponseDraftUpdated {
                status,
                assistant_content,
            } => CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
                status: *status,
                assistant_content: assistant_content.clone(),
            }),
            Self::LlmCallCancelled {
                status,
                bytes_received,
                streaming,
            } => CaptureEvent::LlmCallCancelled(LlmCallCancelled {
                status: *status,
                bytes_received: *bytes_received,
                streaming: *streaming,
            }),
        }
    }
}

fn resp_to_payload(bytes: &Bytes) -> RespPayload {
    if let Ok(s) = std::str::from_utf8(bytes) {
        RespPayload::Text(s.to_string())
    } else {
        RespPayload::Bytes(bytes.to_vec())
    }
}

fn payload_to_bytes(payload: &RespPayload) -> Bytes {
    match payload {
        RespPayload::Text(s) => Bytes::from(s.clone()),
        RespPayload::Bytes(b) => Bytes::from(b.clone()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterReplaySummary {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// Re-apply dead-letter entries through the capture engine (best-effort).
pub async fn replay_dead_letter(
    storage: &Path,
    engine: &crate::engine::CaptureEngine,
) -> Result<DeadLetterReplaySummary> {
    let entries = read_dead_letter_entries(storage)?;
    let storage_arc = Arc::new(storage.to_path_buf());
    let mut summary = DeadLetterReplaySummary {
        attempted: entries.len(),
        succeeded: 0,
        failed: 0,
    };
    for entry in entries {
        let inv = entry.invocation.to_invocation(Arc::clone(&storage_arc));
        let event = entry.event.to_event();
        match engine.apply(&inv, event).await {
            Ok(()) => summary.succeeded += 1,
            Err(e) => {
                summary.failed += 1;
                tracing::warn!("dead letter replay failed: {e:#}");
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_call::CaptureCall;

    fn sample_inv(dir: &Path) -> CaptureInvocation {
        CaptureInvocation {
            route: CaptureRoute {
                root_session: Some("run-1".into()),
                session_id: "sess".into(),
                storage_session_id: "run-1".into(),
                subagent_id: None,
            },
            agent_id: "agent".into(),
            call: CaptureCall {
                call_id: "c1".into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            headers: axum::http::HeaderMap::new(),
            level: CaptureLevel::Dialogue,
            client_model: "m".into(),
            upstream_model: "m".into(),
            provider: ProviderKind::OpenAi,
            protocol: ProtocolKind::ChatCompletions,
            storage: Arc::new(dir.to_path_buf()),
            debug_on: false,
        }
    }

    #[test]
    fn dead_letter_roundtrip_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let inv = sample_inv(dir.path());
        let event = CaptureEvent::LlmRequest(LlmRequestCaptured {
            path: "/v1/chat/completions".into(),
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            model_rewritten: false,
        });
        append_dead_letter(dir.path(), &inv, &event, "mailbox full", None).unwrap();
        let entries = read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error, "mailbox full");
        let restored = entries[0].event.to_event();
        assert!(matches!(restored, CaptureEvent::LlmRequest(_)));
    }
}
