//! AgentCtl v1 Control-plane wire contract.
//!
//! AgentCtl is an optional, cooperative channel between pVisor and runtime
//! clients inside one Run. A client authenticates once with [`AgentRequest::Hello`]
//! and then periodically exchanges its [`AgentState`] for pVisor's current
//! [`AgentDirective`] through [`AgentRequest::Sync`]. Each bounded,
//! newline-delimited JSON connection carries exactly one request and one
//! response.
//!
//! # Session lifecycle and liveness
//!
//! `Hello` requires version 1, the Run-scoped token, and a non-empty
//! `client_id` that is unique among live Sessions. `Welcome` returns an opaque
//! Session ID, the recommended Sync interval, and the directive that the
//! client must observe before admitting work. A successful `Sync` refreshes
//! Session liveness and returns the latest directive.
//!
//! Outside a checkpoint, pVisor considers a Session stale after it misses
//! three recommended Sync intervals. A later `Hello` may replace a stale
//! Session. Live duplicates and capacity conflicts are rejected. Staleness is
//! diagnostic and lifecycle state; it is not proof that the client process
//! stopped.
//!
//! # Checkpoint protocol
//!
//! When pVisor publishes [`AgentDirective::Quiesce`], every runtime Session
//! that was live at checkpoint start must stop accepting work, drain in-flight
//! work, and report [`AgentState::Quiesced`] with the same checkpoint ID.
//! pVisor freezes that participant set, rejects every new or replacement
//! `Hello`, and never expires a participant until the checkpoint completes or
//! is abandoned. Each checkpoint attempt requires a fresh matching report;
//! an acknowledgement retained from an earlier attempt cannot satisfy it.
//!
//! A new `Quiesced` report whose ID does not match the active checkpoint is a
//! conflict. Repeating the exact accepted report is idempotent, including
//! after pVisor publishes `Continue` or `Shutdown`, so a still-quiesced client
//! can learn that new directive. Reporting `Active` or `Idle` while draining
//! removes any current acknowledgement. Clients remain quiesced until they
//! observe [`AgentDirective::Continue`] or [`AgentDirective::Shutdown`]. A
//! missing, late, or ambiguous acknowledgement fails the checkpoint rather
//! than producing an unsafe success.
//!
//! # Errors
//!
//! [`AgentErrorCode::InvalidRequest`] covers malformed frames and values;
//! [`AgentErrorCode::Unauthorized`] covers an invalid Run token or Session ID;
//! [`AgentErrorCode::VersionMismatch`] covers every non-v1 request; and
//! [`AgentErrorCode::Conflict`] covers live duplicates, capacity, checkpoint
//! admission, and state that conflicts with the active checkpoint. The
//! accompanying message is diagnostic text, not a machine contract.
//!
//! # Safety boundary
//!
//! Client states are Agent declarations. They are not enforcement evidence,
//! an authoritative process inventory, or proof that unreported external
//! effects do not exist. pVisor obtains authoritative process facts from its
//! execution provider.
//!
//! # Wire examples
//!
//! A client opens a Session:
//!
//! ```json
//! {"type":"hello","version":1,"token":"run-secret","client_id":"worker-1"}
//! ```
//!
//! It then reports state and receives the current directive:
//!
//! ```json
//! {"type":"sync","version":1,"session_id":"session-1","state":{"kind":"active"}}
//! ```
//!
//! ```json
//! {"type":"synced","directive":{"kind":"quiesce","checkpoint_id":"checkpoint-7"}}
//! ```
//!
//! # Debug plane
//!
//! A future interactive terminal belongs to a separately authorized Debug
//! protocol and Session. PTY input, output, signals, and window size are not
//! extensions of the Control request, response, state, or directive enums.

use serde::{Deserialize, Serialize};

/// The first public AgentCtl Control protocol version.
pub const AGENTCTL_VERSION: u32 = 1;
/// Maximum size of one newline-delimited request or response frame.
pub const AGENTCTL_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Environment variable containing the Run-local AgentCtl endpoint.
pub const AGENTCTL_ENDPOINT_ENV: &str = "PERSISTING_AGENTCTL_ENDPOINT";
/// Environment variable containing the Run-scoped authentication token.
pub const AGENTCTL_TOKEN_ENV: &str = "PERSISTING_AGENTCTL_TOKEN";
/// Environment variable containing [`AGENTCTL_VERSION`].
pub const AGENTCTL_VERSION_ENV: &str = "PERSISTING_AGENTCTL_VERSION";
/// Environment variable naming the transport, currently `unix`.
pub const AGENTCTL_TRANSPORT_ENV: &str = "PERSISTING_AGENTCTL_TRANSPORT";

/// A request sent by a runtime client to pVisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Authenticate a runtime client and create a Session.
    Hello {
        /// Protocol version spoken by the client.
        version: u32,
        /// Run-scoped bearer token injected by pVisor.
        token: String,
        /// Non-empty client identity, unique among live Sessions in the Run.
        client_id: String,
    },
    /// Report current cooperative state and obtain pVisor's current directive.
    Sync {
        /// Protocol version spoken by the client.
        version: u32,
        /// Random Session identifier returned by [`AgentResponse::Welcome`].
        session_id: String,
        /// Client state at the time this request was formed.
        state: AgentState,
    },
}

/// Cooperative state reported by one runtime client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentState {
    /// The client may be running or draining work and is not at a safe point.
    Active,
    /// The client has no current work but may accept work until quiescence.
    Idle,
    /// The client reached the safe boundary for the named checkpoint.
    Quiesced {
        /// Checkpoint whose `Quiesce` directive the client observed.
        checkpoint_id: String,
    },
}

/// Desired cooperative state published by pVisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDirective {
    /// Admit and run ordinary work.
    Continue,
    /// Stop admitting work and reach a safe checkpoint boundary.
    Quiesce {
        /// Identifier clients must echo in [`AgentState::Quiesced`].
        checkpoint_id: String,
        /// Optional absolute deadline for reaching the boundary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_unix_ms: Option<u64>,
    },
    /// Terminate the runtime client.
    Shutdown {
        /// Optional human-readable shutdown reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Stable machine-readable category for an AgentCtl protocol failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    /// The request is malformed or contains invalid values.
    InvalidRequest,
    /// The Run token or Session ID is invalid.
    Unauthorized,
    /// The request does not speak [`AGENTCTL_VERSION`].
    VersionMismatch,
    /// The request conflicts with a live Session or active checkpoint.
    Conflict,
}

/// A response sent by pVisor to a runtime client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    /// Successful Session creation.
    Welcome {
        /// Random bearer identifier for subsequent Sync requests.
        session_id: String,
        /// Recommended period between Sync requests.
        sync_interval_ms: u64,
        /// Current directive, which the client must observe before work starts.
        directive: AgentDirective,
    },
    /// Successful state synchronization.
    Synced {
        /// Current directive after the reported state was accepted.
        directive: AgentDirective,
    },
    /// Rejected request.
    Error {
        /// Stable category used for client control flow.
        code: AgentErrorCode,
        /// Human-readable diagnostic context, not a machine contract.
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn text_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9_ ./:-]{0,32}").unwrap()
    }

    fn state_strategy() -> impl Strategy<Value = AgentState> {
        prop_oneof![
            Just(AgentState::Active),
            Just(AgentState::Idle),
            text_strategy().prop_map(|checkpoint_id| AgentState::Quiesced { checkpoint_id }),
        ]
    }

    fn directive_strategy() -> impl Strategy<Value = AgentDirective> {
        prop_oneof![
            Just(AgentDirective::Continue),
            (text_strategy(), prop::option::of(any::<u64>())).prop_map(
                |(checkpoint_id, deadline_unix_ms)| AgentDirective::Quiesce {
                    checkpoint_id,
                    deadline_unix_ms,
                },
            ),
            prop::option::of(text_strategy())
                .prop_map(|reason| AgentDirective::Shutdown { reason }),
        ]
    }

    fn request_strategy() -> impl Strategy<Value = AgentRequest> {
        prop_oneof![
            (any::<u32>(), text_strategy(), text_strategy()).prop_map(
                |(version, token, client_id)| AgentRequest::Hello {
                    version,
                    token,
                    client_id,
                },
            ),
            (any::<u32>(), text_strategy(), state_strategy()).prop_map(
                |(version, session_id, state)| AgentRequest::Sync {
                    version,
                    session_id,
                    state,
                },
            ),
        ]
    }

    fn response_strategy() -> impl Strategy<Value = AgentResponse> {
        prop_oneof![
            (text_strategy(), any::<u64>(), directive_strategy()).prop_map(
                |(session_id, sync_interval_ms, directive)| AgentResponse::Welcome {
                    session_id,
                    sync_interval_ms,
                    directive,
                },
            ),
            directive_strategy().prop_map(|directive| AgentResponse::Synced { directive }),
            (
                prop_oneof![
                    Just(AgentErrorCode::InvalidRequest),
                    Just(AgentErrorCode::Unauthorized),
                    Just(AgentErrorCode::VersionMismatch),
                    Just(AgentErrorCode::Conflict),
                ],
                text_strategy()
            )
                .prop_map(|(code, message)| AgentResponse::Error { code, message }),
        ]
    }

    #[test]
    fn hello_has_the_v1_wire_shape() {
        let request = AgentRequest::Hello {
            version: 1,
            token: "secret".into(),
            client_id: "worker-1".into(),
        };

        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "type": "hello",
                "version": 1,
                "token": "secret",
                "client_id": "worker-1"
            })
        );
        assert_eq!(AGENTCTL_VERSION, 1);
    }

    #[test]
    fn quiesced_sync_and_quiesce_response_roundtrip() {
        let request = AgentRequest::Sync {
            version: 1,
            session_id: "session-1".into(),
            state: AgentState::Quiesced {
                checkpoint_id: "checkpoint-7".into(),
            },
        };
        let response = AgentResponse::Synced {
            directive: AgentDirective::Quiesce {
                checkpoint_id: "checkpoint-7".into(),
                deadline_unix_ms: Some(1_786_890_000_000),
            },
        };

        assert_eq!(
            serde_json::from_value::<AgentRequest>(serde_json::to_value(&request).unwrap())
                .unwrap(),
            request
        );
        assert_eq!(
            serde_json::from_value::<AgentResponse>(serde_json::to_value(&response).unwrap())
                .unwrap(),
            response
        );
    }

    #[test]
    fn optional_directive_fields_are_omitted() {
        assert_eq!(
            serde_json::to_value(AgentDirective::Continue).unwrap(),
            serde_json::json!({ "kind": "continue" })
        );
        assert_eq!(
            serde_json::to_value(AgentDirective::Shutdown { reason: None }).unwrap(),
            serde_json::json!({ "kind": "shutdown" })
        );
    }

    #[test]
    fn error_codes_use_stable_snake_case_names() {
        let cases = [
            (AgentErrorCode::InvalidRequest, "invalid_request"),
            (AgentErrorCode::Unauthorized, "unauthorized"),
            (AgentErrorCode::VersionMismatch, "version_mismatch"),
            (AgentErrorCode::Conflict, "conflict"),
        ];

        for (code, expected) in cases {
            assert_eq!(serde_json::to_value(code).unwrap(), expected);
        }
    }

    #[test]
    fn welcome_and_error_have_exact_v1_wire_shapes() {
        let welcome = AgentResponse::Welcome {
            session_id: "session-1".into(),
            sync_interval_ms: 1_000,
            directive: AgentDirective::Continue,
        };
        assert_eq!(
            serde_json::to_value(welcome).unwrap(),
            serde_json::json!({
                "type": "welcome",
                "session_id": "session-1",
                "sync_interval_ms": 1_000,
                "directive": { "kind": "continue" }
            })
        );

        let error = AgentResponse::Error {
            code: AgentErrorCode::Conflict,
            message: "checkpoint is active".into(),
        };
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "type": "error",
                "code": "conflict",
                "message": "checkpoint is active"
            })
        );
    }

    #[test]
    fn idle_and_directive_variants_have_exact_v1_wire_shapes() {
        assert_eq!(
            serde_json::to_value(AgentState::Idle).unwrap(),
            serde_json::json!({ "kind": "idle" })
        );
        assert_eq!(
            serde_json::to_value(AgentDirective::Quiesce {
                checkpoint_id: "cp".into(),
                deadline_unix_ms: None,
            })
            .unwrap(),
            serde_json::json!({ "kind": "quiesce", "checkpoint_id": "cp" })
        );
        assert_eq!(
            serde_json::to_value(AgentDirective::Shutdown {
                reason: Some("maintenance".into()),
            })
            .unwrap(),
            serde_json::json!({ "kind": "shutdown", "reason": "maintenance" })
        );
    }

    proptest! {
        #[test]
        fn every_request_roundtrips_through_json(request in request_strategy()) {
            let encoded = serde_json::to_value(&request).unwrap();
            let decoded = serde_json::from_value::<AgentRequest>(encoded).unwrap();
            prop_assert_eq!(decoded, request);
        }

        #[test]
        fn every_response_roundtrips_through_json(response in response_strategy()) {
            let encoded = serde_json::to_value(&response).unwrap();
            let decoded = serde_json::from_value::<AgentResponse>(encoded).unwrap();
            prop_assert_eq!(decoded, response);
        }

        #[test]
        fn optional_directive_fields_follow_presence_semantics(
            checkpoint_id in text_strategy(),
            deadline_unix_ms in prop::option::of(any::<u64>()),
            reason in prop::option::of(text_strategy()),
        ) {
            let quiesce = serde_json::to_value(AgentDirective::Quiesce {
                checkpoint_id,
                deadline_unix_ms: deadline_unix_ms.clone(),
            }).unwrap();
            prop_assert_eq!(
                quiesce.get("deadline_unix_ms").is_some(),
                deadline_unix_ms.is_some(),
            );

            let shutdown = serde_json::to_value(AgentDirective::Shutdown {
                reason: reason.clone(),
            }).unwrap();
            prop_assert_eq!(shutdown.get("reason").is_some(), reason.is_some());
        }
    }
}
