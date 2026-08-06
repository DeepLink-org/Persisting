//! pPilot Supervisor -> pVisor -> OverlayNet integration.

use persisting_control::{RunInvocation, RunSpec, RunState, StdioMode, SupervisorBootstrap};
use persisting_gateway::config::{
    CaptureLevel, NetworkConfig, NetworkMode, OverlayConfig, ProxyConfig,
};
use persisting_ppilot::{EmbeddedSupervisor, EmbeddedSupervisorConfig};
use persisting_pvisor::{GatewayDriverConfig, PVisor};
use std::net::TcpListener as StdTcpListener;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn free_loopback_address() -> String {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address.to_string()
}

async fn spawn_body_server(body_len: usize) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let join = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 16 * 1024);
        }
        let body = vec![b'x'; body_len];
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    (format!("http://{address}/payload"), join)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supervisor_quota_throttles_a_real_pvisor_proxy_request() {
    let cluster_bytes_per_second = 32_768;
    let body_len = 16_384;
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig {
        network_limit_bytes_per_second: Some(cluster_bytes_per_second),
        quota_slots: 2,
    })
    .await
    .unwrap();
    let (url, body_server) = spawn_body_server(body_len).await;
    let workspace = tempfile::tempdir().unwrap();
    let proxy = ProxyConfig {
        listen: free_loopback_address(),
        admin_listen: free_loopback_address(),
        agent_id: "supervisor-limit-test".into(),
        session_header: "x-persisting-session-id".into(),
        capture_level: CaptureLevel::Summary,
        debug: false,
        network: NetworkConfig {
            mode: NetworkMode::Allowlist,
            allowed_hosts: vec!["127.0.0.1".into()],
            rules: Vec::new(),
            deny_rules: Vec::new(),
            limits: Vec::new(),
        },
        overlay: OverlayConfig::default(),
        models: Vec::new(),
    };
    let pvisor = PVisor::builder()
        .storage(workspace.path())
        .gateway(
            GatewayDriverConfig::new(proxy)
                .output_dir(workspace.path())
                .gateway_enabled(true),
        )
        .build();
    let script = format!(
        r#"import os, urllib.request
os.environ['NO_PROXY'] = ''
os.environ['no_proxy'] = ''
proxy = os.environ['HTTP_PROXY']
opener = urllib.request.build_opener(urllib.request.ProxyHandler({{'http': proxy}}))
data = opener.open({url:?}, timeout=8).read()
assert len(data) == {body_len}
print(len(data))
"#
    );
    let mut spec = RunSpec::process("supervisor-limited-run", "fixture-agent", "python3");
    spec.lease_epoch = 1;
    spec.supervisor = Some(supervisor.bootstrap());
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), script];
    process.stdout = StdioMode::Capture;
    process.stderr = StdioMode::Capture;

    let started = Instant::now();
    let result = pvisor.run(spec).await.unwrap().wait().await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(result.state, RunState::Completed, "{result:?}");
    assert_eq!(
        result.output.stdout.as_deref().map(str::trim),
        Some("16384")
    );
    assert!(
        elapsed >= Duration::from_millis(800),
        "Supervisor quota was not applied to OverlayNet: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "limited request stalled: {elapsed:?}"
    );
    body_server.await.unwrap();
    let registrations = supervisor.registrations().await;
    assert_eq!(registrations.len(), 1);
    assert_eq!(registrations[0].run_id.as_str(), "supervisor-limited-run");
    assert!(registrations[0].last_applied_directive_seq > 0);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_supervisor_never_prevents_standalone_execution() {
    let unavailable = free_loopback_address();
    let mut spec = RunSpec::process("supervisor-unavailable", "fixture-agent", "/bin/sh");
    spec.supervisor = Some(SupervisorBootstrap {
        endpoint: format!("tcp://{unavailable}"),
        token: "unavailable".into(),
        controller_epoch: 1,
        connect_timeout_ms: 50,
        attempt_registry_uri: None,
        attempt_ttl_ms: 15_000,
    });
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "printf standalone".into()];
    process.stdout = StdioMode::Capture;
    let result = PVisor::new().run(spec).await.unwrap().wait().await.unwrap();
    assert_eq!(result.state, RunState::Completed);
    assert_eq!(result.output.stdout.as_deref(), Some("standalone"));
    assert!(result
        .warnings
        .iter()
        .any(|warning| warning.contains("continuing standalone")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn supervisor_cancel_reaches_the_live_pvisor_attempt() {
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig::default())
        .await
        .unwrap();
    let mut spec = RunSpec::process("supervisor-cancelled", "fixture-agent", "/bin/sh");
    spec.lease_epoch = 11;
    spec.supervisor = Some(supervisor.bootstrap());
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "sleep 10".into()];
    let handle = PVisor::new().run(spec).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if supervisor
            .registrations()
            .await
            .iter()
            .any(|registration| registration.run_id.as_str() == "supervisor-cancelled")
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    supervisor
        .cancel(&persisting_control::RunId::new("supervisor-cancelled"))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), handle.wait())
        .await
        .expect("Supervisor cancel did not stop the Run")
        .unwrap();
    assert_eq!(result.state, RunState::Cancelled);
    supervisor.shutdown().await.unwrap();
}

#[tokio::test]
async fn losing_a_connected_supervisor_does_not_abort_the_run() {
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig::default())
        .await
        .unwrap();
    let mut spec = RunSpec::process("supervisor-disconnected", "fixture-agent", "/bin/sh");
    spec.supervisor = Some(supervisor.bootstrap());
    let RunInvocation::Process(process) = &mut spec.invocation;
    process.args = vec!["-c".into(), "sleep 0.2; printf survived".into()];
    process.stdout = StdioMode::Capture;
    let handle = PVisor::new().run(spec).await.unwrap();
    supervisor.shutdown().await.unwrap();
    let result = handle.wait().await.unwrap();
    assert_eq!(result.state, RunState::Completed);
    assert_eq!(result.output.stdout.as_deref(), Some("survived"));
}
