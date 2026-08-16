use persisting_agentctl::{
    AgentCtlClient, AgentCtlClientConfig, AgentCtlResponseError, AgentDirective, AgentErrorCode,
    AgentState, AttemptId, RunId,
};
use persisting_pvisor::AgentCtlServer;
use std::time::Duration;

fn client(server: &AgentCtlServer, client_id: &str) -> AgentCtlClient {
    AgentCtlClient::new(
        AgentCtlClientConfig::from_environment(&server.environment(), client_id)
            .unwrap()
            .unwrap(),
    )
}

fn response_code(error: anyhow::Error) -> AgentErrorCode {
    error.downcast::<AgentCtlResponseError>().unwrap().code
}

#[test]
fn all_runtime_clients_must_sync_the_checkpoint_boundary() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let mut first = client(&server, "first");
    let mut second = client(&server, "second");

    assert_eq!(first.connect().unwrap(), AgentDirective::Continue);
    assert_eq!(second.connect().unwrap(), AgentDirective::Continue);
    assert_eq!(
        first.sync(AgentState::Active).unwrap(),
        AgentDirective::Continue
    );
    assert_eq!(
        second.sync(AgentState::Active).unwrap(),
        AgentDirective::Continue
    );

    server
        .control()
        .request_quiesce("checkpoint-1", None)
        .unwrap();
    assert!(matches!(
        first.sync(AgentState::Idle).unwrap(),
        AgentDirective::Quiesce {
            ref checkpoint_id,
            ..
        } if checkpoint_id == "checkpoint-1"
    ));
    first
        .sync(AgentState::Quiesced {
            checkpoint_id: "checkpoint-1".into(),
        })
        .unwrap();

    let snapshot = server.control().snapshot();
    assert!(matches!(
        snapshot
            .clients
            .iter()
            .find(|client| client.client_id == "first")
            .unwrap()
            .state,
        AgentState::Quiesced { ref checkpoint_id } if checkpoint_id == "checkpoint-1"
    ));
    assert!(matches!(
        snapshot
            .clients
            .iter()
            .find(|client| client.client_id == "second")
            .unwrap()
            .state,
        AgentState::Active
    ));

    assert!(matches!(
        second.sync(AgentState::Active).unwrap(),
        AgentDirective::Quiesce { .. }
    ));
    second
        .sync(AgentState::Quiesced {
            checkpoint_id: "checkpoint-1".into(),
        })
        .unwrap();
    assert!(server.control().snapshot().clients.iter().all(|client| {
        matches!(
            &client.state,
            AgentState::Quiesced { checkpoint_id } if checkpoint_id == "checkpoint-1"
        )
    }));

    server.control().continue_execution();
    assert_eq!(
        first
            .sync(AgentState::Quiesced {
                checkpoint_id: "checkpoint-1".into(),
            })
            .unwrap(),
        AgentDirective::Continue
    );
}

#[test]
fn invalid_token_and_duplicate_live_client_have_typed_errors() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let mut bad_config = AgentCtlClientConfig::from_environment(&server.environment(), "bad-token")
        .unwrap()
        .unwrap();
    bad_config.token = "wrong".into();
    assert_eq!(
        response_code(AgentCtlClient::new(bad_config).connect().unwrap_err()),
        AgentErrorCode::Unauthorized
    );

    let mut first = client(&server, "same-client");
    let mut duplicate = client(&server, "same-client");
    first.connect().unwrap();
    assert_eq!(
        response_code(duplicate.connect().unwrap_err()),
        AgentErrorCode::Conflict
    );
}

#[test]
fn checkpoint_rejects_new_sessions_and_mismatched_acknowledgements() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let mut participant = client(&server, "participant");
    participant.connect().unwrap();
    server.control().request_quiesce("cp", None).unwrap();

    let mut late = client(&server, "late");
    assert_eq!(
        response_code(late.connect().unwrap_err()),
        AgentErrorCode::Conflict
    );
    assert_eq!(
        response_code(
            participant
                .sync(AgentState::Quiesced {
                    checkpoint_id: "wrong".into(),
                })
                .unwrap_err()
        ),
        AgentErrorCode::Conflict
    );

    let state = AgentState::Quiesced {
        checkpoint_id: "cp".into(),
    };
    assert!(matches!(
        participant.sync(state.clone()).unwrap(),
        AgentDirective::Quiesce { .. }
    ));
    assert!(matches!(
        participant.sync(state).unwrap(),
        AgentDirective::Quiesce { .. }
    ));
}

#[test]
fn stale_session_can_be_replaced_outside_checkpoint() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let mut stale = client(&server, "worker");
    stale.connect().unwrap();
    std::thread::sleep(Duration::from_millis(3_100));

    let mut replacement = client(&server, "worker");
    assert_eq!(replacement.connect().unwrap(), AgentDirective::Continue);
}

#[test]
fn checkpoint_keeps_a_disconnected_participant_in_the_frozen_set() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let mut participant = client(&server, "participant");
    participant.connect().unwrap();
    server.control().request_quiesce("cp", None).unwrap();
    std::thread::sleep(Duration::from_millis(3_100));

    let snapshot = server.control().snapshot();
    assert_eq!(snapshot.clients.len(), 1);
    assert_eq!(snapshot.clients[0].client_id, "participant");
    assert!(snapshot.clients[0].stale);
}
