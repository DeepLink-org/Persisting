use anyhow::{Context, Result};
use persisting_agentctl::{AttemptId, RunCommitRequest, RunId, RunState};
use persisting_events::{
    ChronicleControl, ChronicleControlEnvelope, ChronicleControlRequest, ChronicleControlResponse,
    ChronicleControlResponseEnvelope, ChronicleServeProcessClient, ChronicleServeReady,
    CommitRunOutcome, LeaseAcquireOutcome, TrajectoryAppendRequest, CHRONICLE_CONTROL_VERSION,
    CHRONICLE_SERVE_READY_VERSION,
};
use persisting_pchronicle::storage::{
    build_storyline_projection, inspect_automatic_storyline_projection,
    probe_canonical_event_store, AutomaticProjectionState, AutomaticProjectionTarget,
    RawEventLanceStore, StoryCoords,
};
use std::future::Future;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;

async fn wait_until<F, Fut>(timeout: Duration, mut condition: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    tokio::time::timeout(timeout, async {
        loop {
            if condition().await? {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("timed out waiting for pChronicle state")??;
    Ok(())
}

async fn append_note(
    storage: &std::path::Path,
    run_id: &str,
    seq: u64,
) -> Result<std::path::PathBuf> {
    let coords = StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        run_id,
        Some(run_id.into()),
    );
    RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: Some(run_id.into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content": format!("{run_id}-{seq}")}),
            }],
        )
        .await?;
    persisting_pchronicle::storage::raw_event_lance_path(&coords)
}

async fn target_for(source: &std::path::Path) -> Result<AutomaticProjectionTarget> {
    let snapshot = probe_canonical_event_store(source.to_string_lossy())
        .await?
        .context("canonical test source")?;
    let run_id = source
        .parent()
        .and_then(std::path::Path::file_name)
        .and_then(|name| name.to_str())
        .context("canonical run path")?;
    Ok(AutomaticProjectionTarget {
        dataset: "default".into(),
        source_path: format!("agent/{run_id}/events.lance"),
        source_uri: snapshot.source_uri.clone(),
        projection_path: format!("agent/{run_id}/storyline"),
        projection_uri: source
            .parent()
            .context("canonical source parent")?
            .join("storyline")
            .to_string_lossy()
            .into_owned(),
        source_snapshot: snapshot,
    })
}

async fn control_call(
    ready: &persisting_events::ChronicleServeControlReady,
    request_id: u64,
    request: ChronicleControlRequest,
) -> Result<ChronicleControlResponse> {
    let mut stream = TcpStream::connect(&ready.endpoint).await?;
    let request = ChronicleControlEnvelope {
        version: CHRONICLE_CONTROL_VERSION,
        request_id,
        auth_token: ready.auth_token.clone(),
        request,
    };
    let mut encoded = serde_json::to_vec(&request)?;
    encoded.push(b'\n');
    stream.write_all(&encoded).await?;
    stream.flush().await?;
    let mut response_line = String::new();
    BufReader::new(stream).read_line(&mut response_line).await?;
    let response: ChronicleControlResponseEnvelope = serde_json::from_str(&response_line)?;
    anyhow::ensure!(
        response.request_id == request_id,
        "control response id mismatch"
    );
    Ok(response.response)
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_readiness_waits_for_projection_and_runtime_discovers_control_appends() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let initial_source = append_note(root.path(), "initial", 0).await?;
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
    tokio::time::timeout(Duration::from_secs(10), stdout.read_line(&mut ready_line))
        .await
        .context("serve readiness timeout")??;
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    let initial = target_for(&initial_source).await?;
    assert_eq!(
        inspect_automatic_storyline_projection(&initial)
            .await?
            .state,
        AutomaticProjectionState::Fresh
    );
    let control = ready.control.context("serve readiness omitted Control")?;

    let runtime_request = TrajectoryAppendRequest {
        storage: root.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: "runtime".into(),
        root_session_id: Some("runtime".into()),
        records: vec![persisting_events::EventRecord {
            identity: Default::default(),
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("runtime".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"runtime"}),
        }],
    };
    assert!(matches!(
        control_call(
            &control,
            21,
            ChronicleControlRequest::AppendTrajectory(runtime_request)
        )
        .await?,
        ChronicleControlResponse::TrajectoryAppend(_)
    ));
    let runtime_source = root.path().join("agent/runtime/events.lance");
    wait_until(Duration::from_secs(10), || {
        let runtime_source = runtime_source.clone();
        async move {
            let Ok(target) = target_for(&runtime_source).await else {
                return Ok(false);
            };
            Ok(inspect_automatic_storyline_projection(&target)
                .await
                .is_ok_and(|inspection| inspection.state == AutomaticProjectionState::Fresh))
        }
    })
    .await?;
    assert!(matches!(
        control_call(&control, 22, ChronicleControlRequest::Ping).await?,
        ChronicleControlResponse::Pong
    ));

    let mut stderr = child.stderr.take().unwrap();
    child.kill().await?;
    let mut diagnostics = String::new();
    stderr.read_to_string(&mut diagnostics).await?;
    assert!(!diagnostics.contains(&control.auth_token));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn serve_startup_foreign_destination_exits_without_readiness_or_mutation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source_a = append_note(root.path(), "a", 0).await?;
    append_note(root.path(), "b", 0).await?;
    let projection_b = root.path().join("agent/b/storyline");
    build_storyline_projection(
        source_a.to_string_lossy(),
        projection_b.to_string_lossy(),
        "agent/a/events.lance",
    )
    .await?;
    let before = std::fs::read(projection_b.join("CURRENT"))?;
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
    let mut ready_line = String::new();
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready_line),
    )
    .await
    .context("foreign startup did not exit")??;
    assert_eq!(bytes, 0);
    assert!(ready_line.is_empty());
    assert!(!child.wait().await?.success());
    assert_eq!(std::fs::read(projection_b.join("CURRENT"))?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_serve_processes_accept_one_fresh_projection_winner() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = append_note(root.path(), "shared", 0).await?;
    let mut children = Vec::new();
    for _ in 0..2 {
        let child = Command::new(env!("CARGO_BIN_EXE_pchronicle"))
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
        children.push(child);
    }
    for child in &mut children {
        let mut ready_line = String::new();
        tokio::time::timeout(
            Duration::from_secs(10),
            BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready_line),
        )
        .await
        .context("concurrent serve readiness timeout")??;
        let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
        assert!(ready.control.is_some());
    }
    let target = target_for(&source).await?;
    assert_eq!(
        inspect_automatic_storyline_projection(&target).await?.state,
        AutomaticProjectionState::Fresh
    );
    for child in &mut children {
        child.kill().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_projection_failure_does_not_stop_control() -> Result<()> {
    let root = tempfile::tempdir()?;
    let owner_source = append_note(root.path(), "owner", 0).await?;
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
    let mut ready_line = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready_line),
    )
    .await??;
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    let control = ready.control.context("serve readiness omitted Control")?;

    let foreign = root.path().join("agent/bad/storyline");
    build_storyline_projection(
        owner_source.to_string_lossy(),
        foreign.to_string_lossy(),
        "agent/owner/events.lance",
    )
    .await?;
    let before = std::fs::read(foreign.join("CURRENT"))?;
    let append = |run_id: &str| TrajectoryAppendRequest {
        storage: root.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: run_id.into(),
        root_session_id: Some(run_id.into()),
        records: vec![persisting_events::EventRecord {
            identity: Default::default(),
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some(run_id.into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content": run_id}),
        }],
    };
    assert!(matches!(
        control_call(
            &control,
            31,
            ChronicleControlRequest::AppendTrajectory(append("bad"))
        )
        .await?,
        ChronicleControlResponse::TrajectoryAppend(_)
    ));

    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = stderr.next_line().await? {
            if line.contains("projection source=agent/bad/events.lance") {
                return Ok::<(), anyhow::Error>(());
            }
        }
        anyhow::bail!("serve stderr closed before projection diagnostic")
    })
    .await
    .context("runtime projection diagnostic timeout")??;
    assert_eq!(std::fs::read(foreign.join("CURRENT"))?, before);
    assert!(matches!(
        control_call(&control, 32, ChronicleControlRequest::Ping).await?,
        ChronicleControlResponse::Pong
    ));
    assert!(matches!(
        control_call(
            &control,
            33,
            ChronicleControlRequest::AppendTrajectory(append("healthy"))
        )
        .await?,
        ChronicleControlResponse::TrajectoryAppend(_)
    ));
    let healthy = root.path().join("agent/healthy/events.lance");
    wait_until(Duration::from_secs(10), || {
        let healthy = healthy.clone();
        async move {
            let Ok(target) = target_for(&healthy).await else {
                return Ok(false);
            };
            Ok(inspect_automatic_storyline_projection(&target)
                .await
                .is_ok_and(|inspection| inspection.state == AutomaticProjectionState::Fresh))
        }
    })
    .await?;
    child.kill().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warehouse_catalog_refreshes_after_control_append_projection() -> Result<()> {
    let root = tempfile::tempdir()?;
    append_note(root.path(), "initial", 0).await?;
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
    tokio::time::timeout(
        Duration::from_secs(10),
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut ready_line),
    )
    .await??;
    let ready: ChronicleServeReady = serde_json::from_str(&ready_line)?;
    let control = ready.control.context("serve readiness omitted Control")?;
    let warehouse = ready
        .warehouse_endpoint
        .context("serve readiness omitted Warehouse")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()?;
    let catalog_url = format!("http://{warehouse}/api/catalog");
    let initial: serde_json::Value = client.get(&catalog_url).send().await?.json().await?;
    let initial_snapshot = initial["snapshot_id"].as_str().unwrap().to_string();

    let request = TrajectoryAppendRequest {
        storage: root.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: "runtime".into(),
        root_session_id: Some("runtime".into()),
        records: vec![persisting_events::EventRecord {
            identity: Default::default(),
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("runtime".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"runtime"}),
        }],
    };
    assert!(matches!(
        control_call(
            &control,
            41,
            ChronicleControlRequest::AppendTrajectory(request)
        )
        .await?,
        ChronicleControlResponse::TrajectoryAppend(_)
    ));
    wait_until(Duration::from_secs(10), || {
        let client = client.clone();
        let catalog_url = catalog_url.clone();
        let initial_snapshot = initial_snapshot.clone();
        async move {
            let catalog: serde_json::Value = client.get(catalog_url).send().await?.json().await?;
            Ok(catalog["snapshot_id"].as_str() != Some(initial_snapshot.as_str()))
        }
    })
    .await?;
    let runtime = target_for(&root.path().join("agent/runtime/events.lance")).await?;
    assert_eq!(
        inspect_automatic_storyline_projection(&runtime)
            .await?
            .state,
        AutomaticProjectionState::Fresh
    );
    child.kill().await?;
    Ok(())
}
