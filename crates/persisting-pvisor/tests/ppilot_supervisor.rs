//! Cross-component Supervisor checks live with the execution plane so pPilot
//! never needs a compile-time dependency on pVisor.

use persisting_agentctl::{RunInvocation, RunSpec, RunState, StdioMode};
use persisting_ppilot::{EmbeddedSupervisor, EmbeddedSupervisorConfig};
use persisting_pvisor::PVisor;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ppilot_supervisor_cancels_a_live_pvisor_attempt() {
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
        .cancel(&persisting_agentctl::RunId::new("supervisor-cancelled"))
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
async fn ppilot_supervisor_disconnect_does_not_abort_the_run() {
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
