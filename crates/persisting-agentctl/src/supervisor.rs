//! Versioned pPilot Supervisor protocol shared by the control and execution planes.

use crate::{AttemptId, NetworkBandwidthLimit, RunId};
use serde::{Deserialize, Serialize};

pub const SUPERVISOR_PROTOCOL_VERSION: u32 = 1;

fn supervisor_protocol_version() -> u32 {
    SUPERVISOR_PROTOCOL_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRegistration {
    #[serde(default = "supervisor_protocol_version")]
    pub protocol_version: u32,
    pub token: String,
    pub run_id: RunId,
    pub attempt_id: AttemptId,
    pub lease_epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorHeartbeat {
    pub last_applied_directive_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorDirectiveAck {
    pub directive_seq: u64,
    pub applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SupervisorClientMessage {
    Register(SupervisorRegistration),
    Heartbeat(SupervisorHeartbeat),
    Ack(SupervisorDirectiveAck),
}

/// A time-bounded rate grant enforced locally by pVisor.
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
    fn wire_messages_roundtrip_as_json() {
        let message = SupervisorServerMessage::Directive(SupervisorDirectiveEnvelope {
            controller_epoch: 4,
            lease_epoch: 9,
            directive_seq: 2,
            directive: SupervisorDirective::Cancel,
        });
        let encoded = serde_json::to_vec(&message).unwrap();
        let decoded: SupervisorServerMessage = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, message);
    }
}
