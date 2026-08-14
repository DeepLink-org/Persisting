//! Versioned wire contract shared by AgentCtl clients and pVisor.

use serde::{Deserialize, Serialize};

pub const AGENT_ABI_VERSION: u32 = 2;
pub const AGENT_ABI_MAX_FRAME_BYTES: usize = 1024 * 1024;

pub const AGENT_ABI_ENDPOINT_ENV: &str = "PERSISTING_AGENT_ABI_ENDPOINT";
pub const AGENT_ABI_TOKEN_ENV: &str = "PERSISTING_AGENT_ABI_TOKEN";
pub const AGENT_ABI_VERSION_ENV: &str = "PERSISTING_AGENT_ABI_VERSION";
pub const AGENT_ABI_TRANSPORT_ENV: &str = "PERSISTING_AGENT_ABI_TRANSPORT";

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
pub struct AgentEffectBegin {
    pub effect_id: String,
    pub kind: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEffectOutcome {
    Committed,
    Aborted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEffectComplete {
    pub effect_id: String,
    pub outcome: AgentEffectOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentRequestBody {
    Hello(AgentHello),
    Heartbeat(AgentLifecycleState),
    RegisterProcess(AgentProcessRegistration),
    CheckpointQuiesced(AgentCheckpointQuiesced),
    EffectBegin(AgentEffectBegin),
    EffectComplete(AgentEffectComplete),
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
            version: AGENT_ABI_VERSION,
            session_id: None,
            body: AgentRequestBody::Hello(hello),
        }
    }

    pub fn authenticated(session_id: impl Into<String>, body: AgentRequestBody) -> Self {
        Self {
            version: AGENT_ABI_VERSION,
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
    EffectAccepted { sequence: u64 },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub body: AgentResponseBody,
}
