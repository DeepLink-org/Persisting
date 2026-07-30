//! Failed capture events — append-only JSONL for recovery (`{storage}/.capture/dead_letter.jsonl`).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::CaptureLevel;
use crate::engine::{
    Call, CallContext, CancelEvent, CompleteEvent, DraftEvent, Event, RequestEvent,
};
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::session_storage::CaptureRoute;
use crate::usage::StreamMetrics;

fn default_post() -> String { "POST".into() }

const DEAD_LETTER_FILENAME: &str = "dead_letter.jsonl";
const TRAJECTORY_DEAD_LETTER_FILENAME: &str = "trajectory_dead_letter.jsonl";

mod serde_compat {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;

    pub mod provider {
        use super::*;

        pub fn serialize<S>(v: &ProviderKind, s: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            s.serialize_str(v.as_str())
        }

        pub fn deserialize<'de, D>(d: D) -> Result<ProviderKind, D::Error>
        where
            D: Deserializer<'de>,
        {
            let s = String::deserialize(d)?;
            Ok(ProviderKind::parse(&s))
        }
    }

    pub mod protocol {
        use super::*;

        pub fn serialize<S>(v: &ProtocolKind, s: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            s.serialize_str(v.as_str())
        }

        pub fn deserialize<'de, D>(d: D) -> Result<ProtocolKind, D::Error>
        where
            D: Deserializer<'de>,
        {
            let s = String::deserialize(d)?;
            Ok(ProtocolKind::parse(&s))
        }
    }
}

/// Serializable call context for dead-letter replay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeadLetterContext {
    pub route: CaptureRoute,
    pub agent_id: String,
    pub call: Call,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_headers: Vec<(String, String)>,
    pub level: CaptureLevel,
    pub client_model: String,
    pub upstream_model: String,
    #[serde(with = "serde_compat::provider")]
    pub provider: ProviderKind,
    #[serde(with = "serde_compat::protocol")]
    pub protocol: ProtocolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_peer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_meta: Option<crate::session_client::SessionClientMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_url: Option<String>,
}

/// Serializable event (wire format for JSONL).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SerializableEvent {
    Request {
        path: String,
        #[serde(default = "default_post")]
        method: String,
        #[serde(default)]
        url: Option<String>,
        body_bytes: usize,
        user_content: Option<String>,
        body_json: Option<Value>,
        model_rewritten: bool,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
    ResponseComplete {
        status: u16,
        resp_payload: RespPayload,
        streaming: bool,
        stream_metrics: Option<StreamMetrics>,
        assistant_content: Option<String>,
        #[serde(default)]
        headers: Vec<(String, String)>,
    },
    ResponseDraft {
        status: u16,
        assistant_content: String,
    },
    Cancelled {
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
    #[serde(alias = "invocation")]
    pub context: DeadLetterContext,
    pub event: SerializableEvent,
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_record_json: Option<String>,
}

pub fn dead_letter_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join(DEAD_LETTER_FILENAME)
}

pub fn trajectory_dead_letter_path(storage: &Path) -> PathBuf {
    storage
        .join(".capture")
        .join(TRAJECTORY_DEAD_LETTER_FILENAME)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrajectoryDeadLetterEntry {
    pub timestamp: String,
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session: Option<String>,
    pub records_ronl: String,
    pub error: String,
}

pub fn append_trajectory_dead_letter(
    storage: &Path,
    agent_id: &str,
    session_id: &str,
    root_session: Option<&str>,
    records_ronl: &str,
    error: &str,
) -> Result<()> {
    let entry = TrajectoryDeadLetterEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        storage: storage.display().to_string(),
        agent_id: agent_id.to_string(),
        session_id: session_id.to_string(),
        root_session: root_session.map(str::to_string),
        records_ronl: records_ronl.to_string(),
        error: error.to_string(),
    };
    let path = trajectory_dead_letter_path(storage);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create .capture for trajectory dead letter")?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open trajectory dead letter {}", path.display()))?;
    let line = serde_json::to_string(&entry).context("serialize trajectory dead letter")?;
    writeln!(file, "{line}").context("append trajectory dead letter")?;
    Ok(())
}

pub fn read_trajectory_dead_letter_entries(
    storage: &Path,
) -> Result<Vec<TrajectoryDeadLetterEntry>> {
    let path = trajectory_dead_letter_path(storage);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("read trajectory dead letter line {i}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(trimmed)
                .with_context(|| format!("parse trajectory dead letter line {i}"))?,
        );
    }
    Ok(out)
}

pub fn append_dead_letter(
    storage: &Path,
    ctx: &CallContext,
    event: &Event,
    error: &str,
    prepared_record_json: Option<String>,
) -> Result<()> {
    let entry = DeadLetterEntry {
        timestamp: chrono::Utc::now().to_rfc3339(),
        context: DeadLetterContext::from_context(ctx),
        event: SerializableEvent::from_event(event),
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

impl DeadLetterContext {
    pub fn from_context(ctx: &CallContext) -> Self {
        Self {
            route: ctx.route().clone(),
            agent_id: ctx.agent_id().to_string(),
            call: ctx.call.clone(),
            request_headers: ctx.request_headers.clone(),
            level: ctx.level,
            client_model: ctx.client_model.clone(),
            upstream_model: ctx.upstream_model.clone(),
            provider: ctx.provider,
            protocol: ctx.protocol,
            client_peer: ctx.client_peer.clone(),
            client_meta: ctx.client_meta.clone(),
            http_version: ctx.http_version.clone(),
            upstream_url: ctx.upstream_url.clone(),
        }
    }

    pub fn to_call_context(&self) -> CallContext {
        let mut ctx = CallContext::new(
            self.route.clone(),
            self.agent_id.clone(),
            self.call.clone(),
            self.request_headers.clone(),
            self.level,
            self.client_model.clone(),
            self.upstream_model.clone(),
            self.provider,
            self.protocol,
            false,
        );
        if let Some(peer) = &self.client_peer {
            ctx.attach_client(peer.clone(), self.client_meta.clone());
        } else if self.client_meta.is_some() {
            ctx.client_meta = self.client_meta.clone();
        }
        if let Some(v) = &self.http_version {
            ctx.attach_http_version(v.clone());
        }
        if let Some(u) = &self.upstream_url {
            ctx.attach_upstream_url(u.clone());
        }
        ctx
    }
}

impl SerializableEvent {
    pub fn from_event(event: &Event) -> Self {
        match event {
            Event::Request(e) => Self::Request {
                path: e.path.clone(),
                method: e.method.clone(),
                url: e.url.clone(),
                body_bytes: e.body_bytes,
                user_content: e.user_content.clone(),
                body_json: e.body_json.clone(),
                model_rewritten: e.model_rewritten,
                headers: e.headers.clone(),
            },
            Event::ResponseComplete(e) => Self::ResponseComplete {
                status: e.status,
                resp_payload: resp_to_payload(&e.resp_bytes),
                streaming: e.streaming,
                stream_metrics: e.stream_metrics.clone(),
                assistant_content: e.assistant_content.clone(),
                headers: e.headers.clone(),
            },
            Event::ResponseDraft(e) => Self::ResponseDraft {
                status: e.status,
                assistant_content: e.assistant_content.clone(),
            },
            Event::Cancelled(e) => Self::Cancelled {
                status: e.status,
                bytes_received: e.bytes_received,
                streaming: e.streaming,
            },
        }
    }

    pub fn to_event(&self) -> Event {
        match self {
            Self::Request {
                path,
                method,
                url,
                body_bytes,
                user_content,
                body_json,
                model_rewritten,
                headers,
            } => Event::Request(RequestEvent {
                path: path.clone(),
                method: method.clone(),
                url: url.clone(),
                body_bytes: *body_bytes,
                user_content: user_content.clone(),
                body_json: body_json.clone(),
                model_rewritten: *model_rewritten,
                headers: headers.clone(),
            }),
            Self::ResponseComplete {
                status,
                resp_payload,
                streaming,
                stream_metrics,
                assistant_content,
                headers,
            } => Event::ResponseComplete(CompleteEvent {
                status: *status,
                resp_bytes: payload_to_bytes(resp_payload),
                streaming: *streaming,
                stream_metrics: stream_metrics.clone(),
                assistant_content: assistant_content.clone(),
                headers: headers.clone(),
            }),
            Self::ResponseDraft {
                status,
                assistant_content,
            } => Event::ResponseDraft(DraftEvent {
                status: *status,
                assistant_content: assistant_content.clone(),
            }),
            Self::Cancelled {
                status,
                bytes_received,
                streaming,
            } => Event::Cancelled(CancelEvent {
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

pub async fn replay_dead_letter(
    storage: &Path,
    engine: &crate::engine::CaptureEngine,
) -> Result<DeadLetterReplaySummary> {
    let entries = read_dead_letter_entries(storage)?;
    let mut summary = DeadLetterReplaySummary {
        attempted: entries.len(),
        succeeded: 0,
        failed: 0,
    };
    for entry in entries {
        let ctx = entry.context.to_call_context();
        let event = entry.event.to_event();
        match engine.apply(&ctx, event).await {
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
    use crate::config::CaptureLevel;
    use crate::protocol::ProtocolKind;
    use crate::provider::ProviderKind;

    fn sample_ctx(_dir: &Path) -> CallContext {
        CallContext::new(
            CaptureRoute {
                root_session: Some("run-1".into()),
                session_id: "sess".into(),
                storage_session_id: "run-1".into(),
                subagent_id: None,
            },
            "agent",
            Call {
                call_id: "c1".into(),
                trace_id: "t1".into(),
                started_at: "2026-01-01T00:00:00Z".into(),
            },
            Vec::new(),
            CaptureLevel::Dialogue,
            "m",
            "m",
            ProviderKind::OpenAi,
            ProtocolKind::ChatCompletions,
            false,
        )
    }

    #[test]
    fn dead_letter_roundtrip_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = sample_ctx(dir.path());
        let event = Event::Request(RequestEvent {
            path: "/v1/chat/completions".into(),
                method: "POST".into(),
                url: None,
            body_bytes: 10,
            user_content: Some("hi".into()),
            body_json: None,
            model_rewritten: false,
                headers: vec![],
            });
        append_dead_letter(dir.path(), &ctx, &event, "mailbox full", None).unwrap();
        let entries = read_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error, "mailbox full");
        assert!(matches!(entries[0].event.to_event(), Event::Request(_)));
        assert_eq!(entries[0].context.provider, ProviderKind::OpenAi);
        assert_eq!(entries[0].context.protocol, ProtocolKind::ChatCompletions);
    }

    #[test]
    fn trajectory_dead_letter_roundtrip_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        append_trajectory_dead_letter(
            dir.path(),
            "agent",
            "sess",
            Some("run-1"),
            "record line\n",
            "engine invoke failed",
        )
        .unwrap();
        let entries = read_trajectory_dead_letter_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].error, "engine invoke failed");
        assert_eq!(entries[0].records_ronl, "record line\n");
    }

    #[test]
    fn dead_letter_context_roundtrips_legacy_string_provider_protocol() {
        let json = r#"{
            "timestamp": "2026-01-01T00:00:00Z",
            "context": {
                "route": {
                    "root_session": "run-1",
                    "session_id": "sess",
                    "storage_session_id": "run-1",
                    "subagent_id": null
                },
                "agent_id": "agent",
                "call": {"call_id": "c1", "trace_id": "t1", "started_at": "2026-01-01T00:00:00Z"},
                "level": "dialogue",
                "client_model": "m",
                "upstream_model": "m",
                "provider": "openai",
                "protocol": "chat_completions"
            },
            "event": {"kind": "cancelled", "status": 499, "bytes_received": 0, "streaming": true},
            "error": "test"
        }"#;
        let entry: DeadLetterEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.context.provider, ProviderKind::OpenAi);
        assert_eq!(entry.context.protocol, ProtocolKind::ChatCompletions);
        let ctx = entry.context.to_call_context();
        assert_eq!(ctx.provider, ProviderKind::OpenAi);
        assert_eq!(ctx.protocol, ProtocolKind::ChatCompletions);
    }
}
