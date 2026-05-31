//! Session lifecycle events for the raw Vortex event log.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::record::{now_rfc3339, CaptureRecord};
use super::session::CaptureRoute;

pub const SESSION_STARTED: &str = "session.started";
pub const SESSION_ENDED: &str = "session.ended";
pub const SESSION_STATE: &str = "session.state";

/// How capture was started (`payload.mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    Run,
    Serve,
    Daemon,
}

impl CaptureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "capture.run",
            Self::Serve => "capture.serve",
            Self::Daemon => "capture.daemon",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLifecyclePayload {
    pub action: String,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

pub fn session_started_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    mode: CaptureMode,
    listen: Option<&str>,
    command: Option<&str>,
) -> CaptureRecord {
    lifecycle_record(
        SESSION_STARTED,
        session_id,
        agent_id,
        SessionLifecyclePayload {
            action: "started".into(),
            mode: mode.as_str().into(),
            listen: listen.map(str::to_string),
            command: command.map(str::to_string),
            exit_code: None,
            reason: None,
            duration_ms: None,
            from: None,
            to: None,
            extra: json!({}),
        },
    )
}

pub fn session_ended_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    mode: CaptureMode,
    reason: &str,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
) -> CaptureRecord {
    lifecycle_record(
        SESSION_ENDED,
        session_id,
        agent_id,
        SessionLifecyclePayload {
            action: "ended".into(),
            mode: mode.as_str().into(),
            listen: None,
            command: None,
            exit_code,
            reason: Some(reason.into()),
            duration_ms,
            from: None,
            to: None,
            extra: json!({}),
        },
    )
}

pub fn session_state_record(
    session_id: Option<String>,
    agent_id: Option<String>,
    mode: CaptureMode,
    from: &str,
    to: &str,
    reason: Option<&str>,
) -> CaptureRecord {
    lifecycle_record(
        SESSION_STATE,
        session_id,
        agent_id,
        SessionLifecyclePayload {
            action: "state".into(),
            mode: mode.as_str().into(),
            listen: None,
            command: None,
            exit_code: None,
            reason: reason.map(str::to_string),
            duration_ms: None,
            from: Some(from.into()),
            to: Some(to.into()),
            extra: json!({}),
        },
    )
}

fn lifecycle_record(
    kind: &str,
    session_id: Option<String>,
    agent_id: Option<String>,
    payload: SessionLifecyclePayload,
) -> CaptureRecord {
    CaptureRecord {
        seq: 0,
        source: "persisting-capture".into(),
        kind: kind.into(),
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
        payload: serde_json::to_value(payload).unwrap_or(json!({})),
    }
}

/// Route for daemon-level proxy lifecycle (`{agent_id}/_proxy/`).
pub fn proxy_lifecycle_route() -> CaptureRoute {
    CaptureRoute {
        root_session: None,
        session_id: "_proxy".into(),
        storage_session_id: "_proxy".into(),
        subagent_id: None,
    }
}

/// Route for a capture run root session directory.
pub fn root_session_route(root_session: &str) -> CaptureRoute {
    CaptureRoute {
        root_session: Some(root_session.into()),
        session_id: root_session.into(),
        storage_session_id: root_session.into(),
        subagent_id: None,
    }
}

pub fn append_lifecycle(
    sink: &dyn super::sink::CaptureSink,
    route: &CaptureRoute,
    agent_id: &str,
    mut record: CaptureRecord,
) -> anyhow::Result<()> {
    sink.append(route, agent_id, &mut record)?;
    Ok(())
}
