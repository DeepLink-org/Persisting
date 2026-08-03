//! Optional pPilot-to-pVisor supervisor control protocol.
//!
//! The protocol is deliberately advisory: failure to connect never prevents a
//! pVisor Run from starting, and loss of the connection never terminates it.

use crate::{AttemptId, NetworkBandwidthLimit, RunId};
use serde::{Deserialize, Serialize};

pub const SUPERVISOR_PROTOCOL_VERSION: u32 = 1;

fn supervisor_protocol_version() -> u32 {
    SUPERVISOR_PROTOCOL_VERSION
}

fn default_supervisor_connect_timeout_ms() -> u64 {
    500
}

/// Connection material injected by pPilot into a RunSpec it launches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorBootstrap {
    pub endpoint: String,
    pub token: String,
    pub controller_epoch: u64,
    #[serde(default = "default_supervisor_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRegistration {
    #[serde(default = "supervisor_protocol_version")]
    pub protocol_version: u32,
    pub token: String,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub lease_epoch: u64,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorHeartbeat {
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub lease_epoch: u64,
    pub last_applied_directive_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorDirectiveAck {
    pub directive_seq: u64,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorClientMessage {
    Register(SupervisorRegistration),
    Heartbeat(SupervisorHeartbeat),
    Ack(SupervisorDirectiveAck),
}

/// A time-bounded rate grant. pVisor enforces it locally on intercepted proxy
/// traffic, so consuming bytes never performs a synchronous control-plane RPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorNetworkQuotaGrant {
    pub grant_id: String,
    pub quota_epoch: u64,
    pub valid_until_unix_ms: u64,
    pub limit: NetworkBandwidthLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisorDirective {
    GrantNetworkQuota(SupervisorNetworkQuotaGrant),
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorDirectiveEnvelope {
    pub controller_epoch: u64,
    pub lease_epoch: u64,
    pub directive_seq: u64,
    pub directive: SupervisorDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorServerMessage {
    Registered {
        controller_epoch: u64,
        directives: Vec<SupervisorDirectiveEnvelope>,
    },
    Directive(SupervisorDirectiveEnvelope),
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages_roundtrip_as_json() {
        let message = SupervisorServerMessage::Directive(SupervisorDirectiveEnvelope {
            controller_epoch: 4,
            lease_epoch: 9,
            directive_seq: 2,
            directive: SupervisorDirective::GrantNetworkQuota(SupervisorNetworkQuotaGrant {
                grant_id: "grant-1".into(),
                quota_epoch: 3,
                valid_until_unix_ms: 100,
                limit: NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 32_768,
                },
            }),
        });
        let encoded = serde_json::to_vec(&message).unwrap();
        let decoded: SupervisorServerMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }
}
