//! Versioned pPilot ↔ pVisor Agent ABI value types.
//!
//! The ABI models Agent-level control and effects rather than Linux syscalls.
//! Transport implementations may use Unix sockets, virtio-vsock, or another
//! ordered request/response channel while retaining the same envelopes.

use serde::{Deserialize, Serialize};

/// Initial wire protocol version for the Agent ABI.
pub const AGENT_ABI_VERSION: u32 = 1;

/// Maximum encoded request or response accepted by the reference transports.
pub const AGENT_ABI_MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    Heartbeat,
    ProcessRegistry,
    CheckpointQuiesce,
    EffectJournal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentClientRole {
    Pilot,
    Agent,
    RuntimeAdapter,
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

/// Desired state published by pVisor and observed through heartbeats.
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
    /// Run-scoped bearer token injected by pVisor beside the endpoint.
    pub auth_token: String,
    /// Stable identifier chosen by the client for this Run.
    pub client_id: String,
    pub role: AgentClientRole,
    pub agent_name: String,
    #[serde(default)]
    pub capabilities: Vec<AgentCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub state: AgentLifecycleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcessRegistration {
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_pid: Option<u32>,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCheckpointQuiesced {
    pub checkpoint_id: String,
    /// Directive generation being acknowledged.
    pub directive_seq: u64,
    /// Highest effect sequence durably observed by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_seq: Option<u64>,
    #[serde(default)]
    pub open_effect_ids: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentRequestBody {
    Hello(AgentHello),
    Heartbeat(AgentHeartbeat),
    RegisterProcess(AgentProcessRegistration),
    CheckpointQuiesced(AgentCheckpointQuiesced),
    EffectBegin(AgentEffectBegin),
    EffectComplete(AgentEffectComplete),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequest {
    #[serde(default = "agent_abi_version")]
    pub version: u32,
    pub request_id: String,
    /// Issued by the Hello response and required for all later requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub body: AgentRequestBody,
}

impl AgentRequest {
    pub fn hello(request_id: impl Into<String>, hello: AgentHello) -> Self {
        Self {
            version: AGENT_ABI_VERSION,
            request_id: request_id.into(),
            session_id: None,
            body: AgentRequestBody::Hello(hello),
        }
    }

    pub fn authenticated(
        request_id: impl Into<String>,
        session_id: impl Into<String>,
        body: AgentRequestBody,
    ) -> Self {
        Self {
            version: AGENT_ABI_VERSION,
            request_id: request_id.into(),
            session_id: Some(session_id.into()),
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWelcome {
    pub session_id: String,
    pub run_id: String,
    pub attempt_id: String,
    #[serde(default)]
    pub accepted_capabilities: Vec<AgentCapability>,
    pub heartbeat_interval_ms: u64,
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeatAck {
    pub server_time_unix_ms: u64,
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAck {
    pub accepted_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEffectAccepted {
    pub sequence: u64,
    pub accepted_at_unix_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAbiErrorCode {
    VersionMismatch,
    Unauthorized,
    InvalidSession,
    CapabilityNotNegotiated,
    InvalidTransition,
    MalformedRequest,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAbiError {
    pub code: AgentAbiErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentResponseBody {
    Welcome(AgentWelcome),
    Heartbeat(AgentHeartbeatAck),
    Ack(AgentAck),
    EffectAccepted(AgentEffectAccepted),
    Error(AgentAbiError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub version: u32,
    pub request_id: String,
    pub body: AgentResponseBody,
}

const fn agent_abi_version() -> u32 {
    AGENT_ABI_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_json_roundtrip_has_stable_discriminator() {
        let request = AgentRequest::hello(
            "req-1",
            AgentHello {
                auth_token: "secret".into(),
                client_id: "pilot-1".into(),
                role: AgentClientRole::Pilot,
                agent_name: "test-agent".into(),
                capabilities: vec![AgentCapability::Heartbeat],
            },
        );
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains(r#""type":"hello""#));
        assert_eq!(
            serde_json::from_str::<AgentRequest>(&encoded).unwrap(),
            request
        );
    }

    #[test]
    fn directive_roundtrip_preserves_checkpoint_identity() {
        let directive = AgentDirective::Quiesce {
            checkpoint_id: "checkpoint-7".into(),
            deadline_unix_ms: Some(42),
        };
        let encoded = serde_json::to_vec(&directive).unwrap();
        assert_eq!(
            serde_json::from_slice::<AgentDirective>(&encoded).unwrap(),
            directive
        );
    }
}
