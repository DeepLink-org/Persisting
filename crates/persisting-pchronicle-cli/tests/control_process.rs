use anyhow::Result;
use persisting_agentctl::{AttemptId, RunCommitRequest, RunId, RunState};
use persisting_events::{
    ChronicleControl, ChronicleControlEnvelope, ChronicleControlRequest, ChronicleControlResponse,
    ChronicleControlResponseEnvelope, ChronicleServeProcessClient, ChronicleServeReady,
    CommitRunOutcome, LeaseAcquireOutcome, TrajectoryAppendRequest, CHRONICLE_CONTROL_VERSION,
    CHRONICLE_SERVE_READY_VERSION,
};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_control_only_advertises_no_warehouse_and_accepts_ping() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .arg("serve")
        .arg("--storage")
        .arg(root.path())
        .arg("--control")
        .arg("127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ready_line = String::new();
    assert!(stdout.read_line(&mut ready_line).await? > 0);
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    assert_eq!(ready.version, CHRONICLE_SERVE_READY_VERSION);
    assert!(ready.warehouse_endpoint.is_none());
    assert!(ready.gateway_endpoint.is_none());
    assert!(ready.gateway_admin_endpoint.is_none());
    let control = ready.control.unwrap();

    let mut stream = TcpStream::connect(&control.endpoint).await?;
    let request = ChronicleControlEnvelope {
        version: CHRONICLE_CONTROL_VERSION,
        request_id: 11,
        auth_token: control.auth_token.clone(),
        request: ChronicleControlRequest::Ping,
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line).await?;
    let response: ChronicleControlResponseEnvelope = serde_json::from_str(&response_line)?;
    assert_eq!(response.request_id, 11);
    assert!(matches!(response.response, ChronicleControlResponse::Pong));

    let mut stderr = child.stderr.take().unwrap();
    child.kill().await?;
    let mut diagnostics = String::new();
    stderr.read_to_string(&mut diagnostics).await?;
    assert!(!diagnostics.contains(&control.auth_token));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_gateway_only_advertises_no_warehouse_or_control() -> Result<()> {
    let root = tempfile::tempdir()?;
    let gateway_config = root.path().join("gateway.toml");
    std::fs::write(
        &gateway_config,
        r#"
listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
agent_id = "gateway-only"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .arg("serve")
        .arg("--storage")
        .arg(root.path())
        .arg("--gateway")
        .arg(&gateway_config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut ready_line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready_line)
        .await?;
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    assert!(ready.warehouse_endpoint.is_none());
    assert!(ready.control.is_none());
    assert!(ready.gateway_endpoint.is_some());
    assert!(ready.gateway_admin_endpoint.is_some());

    child.kill().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_can_host_warehouse_and_control_together() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
        .arg("serve")
        .arg("--storage")
        .arg(root.path())
        .arg("--listen")
        .arg("127.0.0.1:0")
        .arg("--control")
        .arg("127.0.0.1:0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut ready_line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready_line)
        .await?;
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    assert!(ready.warehouse_endpoint.is_some());
    assert!(ready.gateway_endpoint.is_none());
    assert!(ready.gateway_admin_endpoint.is_none());
    let control = ready.control.unwrap();

    let mut stream = TcpStream::connect(&control.endpoint).await?;
    let request = ChronicleControlEnvelope {
        version: CHRONICLE_CONTROL_VERSION,
        request_id: 12,
        auth_token: control.auth_token,
        request: ChronicleControlRequest::Ping,
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line).await?;
    let response: ChronicleControlResponseEnvelope = serde_json::from_str(&response_line)?;
    assert_eq!(response.request_id, 12);
    assert!(matches!(response.response, ChronicleControlResponse::Pong));

    child.kill().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_process_owns_run_state_and_trajectory_append() -> Result<()> {
    let root = tempfile::tempdir()?;
    let client = ChronicleServeProcessClient::spawn(
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
