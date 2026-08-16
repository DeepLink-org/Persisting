use anyhow::Result;
use persisting_agentctl::{AttemptId, RunCommitRequest, RunId, RunState};
use persisting_events::{
    ChronicleControl, ChronicleControlProcessClient, CommitRunOutcome, LeaseAcquireOutcome,
    TrajectoryAppendRequest,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_process_owns_run_state_and_trajectory_append() -> Result<()> {
    let root = tempfile::tempdir()?;
    let client = ChronicleControlProcessClient::spawn(
        env!("CARGO_BIN_EXE_pchronicle"),
        root.path().to_string_lossy(),
    )
    .await?;
    let run_id = RunId::new("run-control-process");
    let lease = client
        .acquire_lease(&run_id, Some("task-1"), "pilot-1", 30_000)
        .await?;
    let LeaseAcquireOutcome::Acquired(lease) = lease else {
        panic!("first lease must be acquired")
    };
    let attempt_id = AttemptId::new("attempt-1");
    assert!(
        client
            .bind_attempt(&run_id, lease.epoch, attempt_id.clone())
            .await?
    );
    assert!(
        client
            .publish_attempt_active(run_id.as_str(), attempt_id.as_str(), lease.epoch, 30_000)
            .await?
    );
    assert!(client.get_attempt(run_id.as_str()).await?.is_some());

    let committed = client
        .commit_run(RunCommitRequest {
            run_id: run_id.clone(),
            task_id: Some("task-1".into()),
            attempt_id,
            lease_epoch: lease.epoch,
            state: RunState::Completed,
            event_high_watermark: None,
            result_digest: "sha256:test".into(),
        })
        .await?;
    assert!(matches!(committed, CommitRunOutcome::Committed(_)));
    assert!(client.get_run(&run_id).await?.unwrap().commit.is_some());
    assert_eq!(client.list_runs().await?.len(), 1);

    let appended = client
        .append_trajectory(TrajectoryAppendRequest {
            storage: root.path().join("traj").display().to_string(),
            agent_id: "ppilot".into(),
            session_id: "session-1".into(),
            root_session_id: None,
            records: Vec::new(),
        })
        .await?;
    assert_eq!(appended.accepted_records, 0);
    assert_eq!(appended.status, "ok");
    Ok(())
}
