use persisting_agentctl::{
    checkpoint_directive, AgentCheckpointQuiesced, AgentClientRole, AgentCtlClient,
    AgentCtlClientConfig, AgentLifecycleState, AgentOperationBegin, AgentOperationComplete,
    AgentOperationOutcome, AgentProcessRegistration, AttemptId, RunId,
};
use persisting_pvisor::AgentCtlServer;

#[test]
fn client_drives_process_effect_and_quiescence_lifecycle() {
    let server = AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
    let config = AgentCtlClientConfig::from_environment(
        &server.environment(),
        "agent-1",
        AgentClientRole::Agent,
        "example-agent",
    )
    .unwrap()
    .unwrap();
    let mut client = AgentCtlClient::new(config);
    client.connect().unwrap();
    client
        .register_process(AgentProcessRegistration {
            pid: 7,
            role: "agent".into(),
            executable: Some("example-agent".into()),
        })
        .unwrap();
    client
        .begin_operation(AgentOperationBegin {
            operation_id: "effect-1".into(),
            kind: "tool.call".into(),
            request_digest: "sha256:abc".into(),
            idempotency_key: Some("idem-1".into()),
        })
        .unwrap();
    client
        .complete_operation(AgentOperationComplete {
            operation_id: "effect-1".into(),
            outcome: AgentOperationOutcome::Committed,
        })
        .unwrap();
    let directive_seq = server.control().request_quiesce("checkpoint-1", None);
    let ack = client.heartbeat(AgentLifecycleState::Quiescing).unwrap();
    assert_eq!(
        checkpoint_directive(&ack),
        Some(("checkpoint-1", directive_seq))
    );
    client
        .checkpoint_quiesced(AgentCheckpointQuiesced {
            checkpoint_id: "checkpoint-1".into(),
            directive_seq,
        })
        .unwrap();
    let snapshot = server.control().snapshot();
    assert_eq!(snapshot.clients[0].client_id, "agent-1");
    assert_eq!(snapshot.processes[0].registration.pid, 7);
    assert_eq!(
        snapshot.operations[0].completion.as_ref().unwrap().outcome,
        AgentOperationOutcome::Committed
    );
}
