//! Versioned AgentCtl wire contract shared by clients and pVisor.
//!
//! AgentCtl is a cooperative Run-local control channel. Its process and
//! operation reports are Agent declarations; they are not enforcement evidence
//! and are not an authoritative inventory of external effects.

use serde::{Deserialize, Serialize};

pub const AGENTCTL_VERSION: u32 = 2;
pub const AGENTCTL_MAX_FRAME_BYTES: usize = 1024 * 1024;

pub const AGENTCTL_ENDPOINT_ENV: &str = "PERSISTING_AGENTCTL_ENDPOINT";
pub const AGENTCTL_TOKEN_ENV: &str = "PERSISTING_AGENTCTL_TOKEN";
pub const AGENTCTL_VERSION_ENV: &str = "PERSISTING_AGENTCTL_VERSION";
pub const AGENTCTL_TRANSPORT_ENV: &str = "PERSISTING_AGENTCTL_TRANSPORT";

/// Legacy environment names accepted during the Agent ABI to AgentCtl migration.
pub const LEGACY_AGENT_ABI_ENDPOINT_ENV: &str = "PERSISTING_AGENT_ABI_ENDPOINT";
pub const LEGACY_AGENT_ABI_TOKEN_ENV: &str = "PERSISTING_AGENT_ABI_TOKEN";
pub const LEGACY_AGENT_ABI_VERSION_ENV: &str = "PERSISTING_AGENT_ABI_VERSION";
pub const LEGACY_AGENT_ABI_TRANSPORT_ENV: &str = "PERSISTING_AGENT_ABI_TRANSPORT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentClientRole {
    Pilot,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleState {
    Starting,
    Running,
    Idle,
    Quiescing,
    Quiesced,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDirective {
    Continue,
    Quiesce {
        checkpoint_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_unix_ms: Option<u64>,
    },
    Shutdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub auth_token: String,
    pub client_id: String,
    pub role: AgentClientRole,
    pub agent_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcessRegistration {
    pub pid: u32,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCheckpointQuiesced {
    pub checkpoint_id: String,
    pub directive_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOperationBegin {
    #[serde(rename = "effect_id")]
    pub operation_id: String,
    pub kind: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOperationOutcome {
    Committed,
    Aborted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOperationComplete {
    #[serde(rename = "effect_id")]
    pub operation_id: String,
    pub outcome: AgentOperationOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentRequestBody {
    Hello(AgentHello),
    Heartbeat(AgentLifecycleState),
    RegisterProcess(AgentProcessRegistration),
    CheckpointQuiesced(AgentCheckpointQuiesced),
    /// Agent-declared open operation used only for cooperative quiescence.
    EffectBegin(AgentOperationBegin),
    /// Completion of an Agent-declared operation.
    EffectComplete(AgentOperationComplete),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub body: AgentRequestBody,
}

impl AgentRequest {
    pub fn hello(hello: AgentHello) -> Self {
        Self {
            version: AGENTCTL_VERSION,
            session_id: None,
            body: AgentRequestBody::Hello(hello),
        }
    }

    pub fn authenticated(session_id: impl Into<String>, body: AgentRequestBody) -> Self {
        Self {
            version: AGENTCTL_VERSION,
            session_id: Some(session_id.into()),
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWelcome {
    pub session_id: String,
    pub heartbeat_interval_ms: u64,
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeatAck {
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentResponseBody {
    Welcome(AgentWelcome),
    Heartbeat(AgentHeartbeatAck),
    Ack,
    #[serde(rename = "effect_accepted")]
    OperationAccepted {
        sequence: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub body: AgentResponseBody,
}

// Source compatibility for clients compiled against the former Agent ABI
// vocabulary. New code should use the AgentCtl names above.
#[deprecated(note = "use AGENTCTL_VERSION")]
pub const AGENT_ABI_VERSION: u32 = AGENTCTL_VERSION;
#[deprecated(note = "use AGENTCTL_MAX_FRAME_BYTES")]
pub const AGENT_ABI_MAX_FRAME_BYTES: usize = AGENTCTL_MAX_FRAME_BYTES;
#[deprecated(note = "use LEGACY_AGENT_ABI_ENDPOINT_ENV only for migration")]
pub const AGENT_ABI_ENDPOINT_ENV: &str = LEGACY_AGENT_ABI_ENDPOINT_ENV;
#[deprecated(note = "use LEGACY_AGENT_ABI_TOKEN_ENV only for migration")]
pub const AGENT_ABI_TOKEN_ENV: &str = LEGACY_AGENT_ABI_TOKEN_ENV;
#[deprecated(note = "use LEGACY_AGENT_ABI_VERSION_ENV only for migration")]
pub const AGENT_ABI_VERSION_ENV: &str = LEGACY_AGENT_ABI_VERSION_ENV;
#[deprecated(note = "use LEGACY_AGENT_ABI_TRANSPORT_ENV only for migration")]
pub const AGENT_ABI_TRANSPORT_ENV: &str = LEGACY_AGENT_ABI_TRANSPORT_ENV;
#[deprecated(note = "use AgentOperationBegin")]
pub type AgentEffectBegin = AgentOperationBegin;
#[deprecated(note = "use AgentOperationComplete")]
pub type AgentEffectComplete = AgentOperationComplete;
#[deprecated(note = "use AgentOperationOutcome")]
pub type AgentEffectOutcome = AgentOperationOutcome;
