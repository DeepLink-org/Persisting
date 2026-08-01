//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Callers configure a [`PVisor`] and invoke [`PVisor::run`]. There is no
//! separate control plane: CLI / pPilot talk to this API directly.

use crate::config::{GatewayDriverConfig, PVisorConfig};
use crate::event::{EventSink, NoopEventSink, RunEventPublisher};
use crate::executor::{AttemptContext, RunExecutor};
use crate::process::ProcessExecutor;
use crate::runtime::{
    ImplantPlan, OverlayHint, RuntimeCapabilities, RuntimeSupervisor, RuntimeSupervisorBuilder,
};
use crate::util::unix_now_ms;
use crate::TrajectoryEventSink;
use persisting_control::ControlController;
use persisting_proto::{
    AttemptId, AttemptInfo, EventEnvelope, PolicyMode, RunResult, RunSpec, RunState, RunStatus,
    RUNTIME_SCHEMA_VERSION,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum PVisorError {
    #[error("invalid RunSpec: {0}")]
    InvalidSpec(String),
    #[error("runtime prepare failed: {0}")]
    Prepare(#[source] anyhow::Error),
    #[error("no executor supports this invocation")]
    UnsupportedInvocation,
    #[error("executor `{0}` cannot enforce the requested capability policy")]
    UnsupportedPolicy(String),
    #[error("event sink rejected run creation: {0}")]
    EventSink(#[source] anyhow::Error),
    #[error("run task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type RunEventStream = broadcast::Receiver<EventEnvelope>;

/// Handle for one in-flight Run: status, cancel, wait, event subscribe.
pub struct RunHandle {
    run_id: persisting_proto::RunId,
    attempt_id: AttemptId,
    status: watch::Receiver<RunStatus>,
    cancellation: CancellationToken,
    events: RunEventPublisher,
    join: JoinHandle<RunResult>,
}

impl RunHandle {
    pub fn run_id(&self) -> &persisting_proto::RunId {
        &self.run_id
    }

    pub fn attempt_id(&self) -> &AttemptId {
        &self.attempt_id
    }

    pub fn status(&self) -> RunStatus {
        self.status.borrow().clone()
    }

    pub async fn status_changed(&mut self) -> Option<RunStatus> {
        self.status.changed().await.ok()?;
        Some(self.status())
    }

    pub fn subscribe_events(&self) -> RunEventStream {
        self.events.subscribe()
    }

    /// Cooperative cancel followed by executor-specific termination.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn wait(self) -> Result<RunResult, PVisorError> {
        Ok(self.join.await?)
    }
}

/// Builder for a configured [`PVisor`].
#[derive(Clone, Default)]
pub struct PVisorBuilder {
    runtime: RuntimeSupervisorBuilder,
    event_sink: Option<Arc<dyn EventSink>>,
    executors: Option<Vec<Arc<dyn RunExecutor>>>,
}

impl std::fmt::Debug for PVisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PVisorBuilder")
            .field("runtime", &self.runtime)
            .field(
                "event_sink",
                &self.event_sink.as_ref().map(|_| "<EventSink>"),
            )
            .field("executors", &self.executors.as_ref().map(|e| e.len()))
            .finish()
    }
}

impl PVisorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply the top-level pVisor configuration.
    pub fn config(mut self, config: PVisorConfig) -> Self {
        if let Some(gateway) = config.gateway {
            self.runtime = self.runtime.gateway(gateway);
        }
        self.runtime = self.runtime.overlay(config.overlay);
        self
    }

    /// Enable pVisor's built-in Agent protocol Gateway driver.
    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.runtime = self.runtime.gateway(gateway);
        self
    }

    /// Inject the structured trajectory output port used by the Gateway driver.
    pub fn trajectory_sink(mut self, sink: Arc<dyn TrajectoryEventSink>) -> Self {
        self.runtime = self.runtime.trajectory_sink(sink);
        self
    }

    pub fn overlay(mut self, overlay: OverlayHint) -> Self {
        self.runtime = self.runtime.overlay(overlay);
        self
    }

    /// Set durable Run workspace storage independently of the optional Gateway.
    pub fn storage(mut self, storage: impl Into<std::path::PathBuf>) -> Self {
        self.runtime = self.runtime.storage(storage.into());
        self
    }

    pub fn control_controller(mut self, controller: Arc<dyn ControlController>) -> Self {
        self.runtime = self.runtime.control_controller(controller);
        self
    }

    pub fn event_sink(mut self, event_sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(event_sink);
        self
    }

    pub fn executors(mut self, executors: Vec<Arc<dyn RunExecutor>>) -> Self {
        self.executors = Some(executors);
        self
    }

    pub fn build(self) -> PVisor {
        PVisor {
            executors: Arc::new(
                self.executors
                    .unwrap_or_else(|| vec![Arc::new(ProcessExecutor) as Arc<dyn RunExecutor>]),
            ),
            event_sink: self
                .event_sink
                .unwrap_or_else(|| Arc::new(NoopEventSink) as Arc<dyn EventSink>),
            runtime: self.runtime.build(),
        }
    }
}

/// Portable Agent execution runtime.
///
/// Owns Attempt prepare (capture / network / overlay), process execution, and
/// Run lifecycle. Hosts call [`Self::run`] directly — no forwarding control plane.
#[derive(Clone)]
pub struct PVisor {
    executors: Arc<Vec<Arc<dyn RunExecutor>>>,
    event_sink: Arc<dyn EventSink>,
    runtime: RuntimeSupervisor,
}

impl Default for PVisor {
    fn default() -> Self {
        Self::new()
    }
}

impl PVisor {
    pub fn new() -> Self {
        Self::builder().build()
    }

    pub fn builder() -> PVisorBuilder {
        PVisorBuilder::new()
    }

    pub fn capabilities(&self) -> RuntimeCapabilities {
        self.runtime.capabilities()
    }

    /// Dry-run implant plan (env / network markers) without starting capture.
    pub fn plan_for(&self, spec: &RunSpec) -> ImplantPlan {
        self.runtime.plan_for(spec)
    }

    /// Start one Run: prepare controls → execute → teardown on completion.
    pub async fn run(&self, spec: RunSpec) -> Result<RunHandle, PVisorError> {
        self.submit(spec).await
    }

    /// Alias for [`Self::run`].
    pub async fn submit(&self, mut spec: RunSpec) -> Result<RunHandle, PVisorError> {
        validate_spec(&spec)?;
        let executor = self
            .executors
            .iter()
            .find(|executor| executor.supports(&spec.invocation))
            .cloned()
            .ok_or(PVisorError::UnsupportedInvocation)?;
        let descriptor = executor.descriptor();
        if spec.runtime.policy_mode == PolicyMode::Enforce && !descriptor.enforces_capabilities {
            return Err(PVisorError::UnsupportedPolicy(descriptor.name));
        }
        let session = self
            .runtime
            .prepare(&mut spec)
            .map_err(PVisorError::Prepare)?;

        let run_id = spec.run_id.clone();
        let attempt_id = AttemptId::new(format!("attempt-{}", uuid::Uuid::new_v4()));
        let now = unix_now_ms();
        let initial = RunStatus {
            run_id: run_id.clone(),
            state: RunState::Created,
            attempt: AttemptInfo {
                attempt_id: attempt_id.clone(),
                number: 0,
                executor: descriptor.clone(),
                started_at_unix_ms: None,
                finished_at_unix_ms: None,
            },
            updated_at_unix_ms: now,
            message: None,
        };
        let (status_tx, status_rx) = watch::channel(initial);
        let (live_tx, _) = broadcast::channel(256);
        let events = RunEventPublisher::new(
            run_id.clone(),
            attempt_id.clone(),
            "persisting-pvisor",
            Arc::clone(&self.event_sink),
            live_tx,
        );
        events
            .publish(
                "run.created",
                "runtime",
                json!({
                    "agent": spec.agent,
                    "task_id": spec.task_id,
                    "executor": descriptor,
                    "policy_mode": spec.runtime.policy_mode,
                    "capture_session": session.as_ref().map(|s| s.root_session.clone()),
                }),
            )
            .await
            .map_err(PVisorError::EventSink)?;

        let cancellation = CancellationToken::new();
        let context = AttemptContext::new(
            Arc::new(spec),
            attempt_id.clone(),
            cancellation.clone(),
            status_tx,
            events.clone(),
        );
        let join = tokio::spawn(async move {
            let mut result = executor.execute(context.clone()).await;
            if let Some(session) = session {
                if let Err(err) = session.teardown(result.exit_code) {
                    result
                        .warnings
                        .push(format!("attempt session teardown failed: {err:#}"));
                }
            }
            context.transition(result.state, None).await;
            let kind = match result.state {
                RunState::Completed => "run.completed",
                RunState::Cancelled => "run.cancelled",
                _ => "run.failed",
            };
            if let Err(error) = context
                .events()
                .publish(
                    kind,
                    "runtime",
                    json!({
                        "state": result.state,
                        "exit_code": result.exit_code,
                        "failure": result.failure,
                        "started_at_unix_ms": result.started_at_unix_ms,
                        "finished_at_unix_ms": result.finished_at_unix_ms,
                    }),
                )
                .await
            {
                result
                    .warnings
                    .push(format!("terminal event sink failed: {error:#}"));
            }
            result
        });

        Ok(RunHandle {
            run_id,
            attempt_id,
            status: status_rx,
            cancellation,
            events,
            join,
        })
    }
}

fn validate_spec(spec: &RunSpec) -> Result<(), PVisorError> {
    if spec.schema_version != RUNTIME_SCHEMA_VERSION {
        return Err(PVisorError::InvalidSpec(format!(
            "unsupported schema_version {}; expected {}",
            spec.schema_version, RUNTIME_SCHEMA_VERSION
        )));
    }
    if spec.run_id.is_empty() {
        return Err(PVisorError::InvalidSpec("run_id must not be empty".into()));
    }
    if spec.agent.name.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "agent.name must not be empty".into(),
        ));
    }
    let persisting_proto::RunInvocation::Process(process) = &spec.invocation;
    if process.program.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "process program must not be empty".into(),
        ));
    }
    if process.stdin == persisting_proto::StdioMode::Capture {
        return Err(PVisorError::InvalidSpec(
            "captured stdin is not supported in pVisor v1".into(),
        ));
    }
    if spec.runtime.max_output_bytes == 0 {
        return Err(PVisorError::InvalidSpec(
            "runtime.max_output_bytes must be greater than zero".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryEventSink;
    use persisting_proto::{NetworkCapability, RunFailureKind, RunInvocation, StdioMode};

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_completes_and_emits_lifecycle() {
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-success", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "printf pvisor".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.run(spec).await.unwrap();
        assert_eq!(handle.status().state, RunState::Created);
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(result.output.stdout.as_deref(), Some("pvisor"));

        let kinds: Vec<_> = sink.events().into_iter().map(|event| event.kind).collect();
        assert_eq!(kinds.first().map(String::as_str), Some("run.created"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.completed"));
        assert!(kinds.iter().any(|kind| kind == "run.state_changed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_run_cancels_the_process_tree() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-cancel", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "sleep 30 & wait".into()];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;

        let handle = runtime.run(spec).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        handle.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), handle.wait())
            .await
            .expect("process tree did not terminate")
            .unwrap();
        assert_eq!(result.state, RunState::Cancelled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_deadline_is_a_typed_failure() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-timeout", "test-agent", "/bin/sleep");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["30".into()];
        process.stdout = StdioMode::Null;
        process.stderr = StdioMode::Null;
        spec.runtime.timeout_ms = Some(20);

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(
            result.failure.unwrap().kind,
            RunFailureKind::DeadlineExceeded
        );
    }

    #[tokio::test]
    async fn host_process_refuses_enforced_policy() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-enforce", "test-agent", "echo");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        let error = match runtime.run(spec).await {
            Ok(_) => panic!("host process must not claim capability enforcement"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::UnsupportedPolicy(_)));
    }

    #[tokio::test]
    async fn gateway_driver_does_not_elevate_host_process_enforcement() {
        let proxy = persisting_gateway::config::ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:19081"
admin_listen = "127.0.0.1:9876"
agent_id = "test"

[[models]]
name = "*"
upstream = "https://example.com"
"#,
        )
        .unwrap();
        let runtime = PVisor::builder()
            .gateway(GatewayDriverConfig::new(proxy))
            .build();
        let mut spec = RunSpec::process("run-enforce-capture", "test-agent", "echo");
        spec.runtime.policy_mode = PolicyMode::Enforce;
        spec.capabilities.network = NetworkCapability::Deny;
        let error = match runtime.run(spec).await {
            Ok(_) => panic!("explicit proxy capture cannot enforce host process capabilities"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::UnsupportedPolicy(_)));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires an enabled macFUSE kernel extension"]
    async fn overlay_run_does_not_require_gateway() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        let storage = temporary.path().join("storage");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("base.txt"), b"base").unwrap();

        let pvisor = PVisor::builder()
            .storage(&storage)
            .overlay(OverlayHint {
                lower_dirs: vec![target.clone()],
                ..OverlayHint::default()
            })
            .build();
        let mut spec = RunSpec::process("run-overlay-only", "agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "test \"$(cat base.txt)\" = base && printf changed > base.txt && printf new > new.txt"
                .into(),
        ];

        let result = pvisor.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(std::fs::read(target.join("base.txt")).unwrap(), b"base");
        assert!(!target.join("new.txt").exists());

        let record =
            crate::runtime::resolve_run(Some(std::path::Path::new("run-overlay-only")), &storage)
                .unwrap();
        assert!(record.gateway_listen.is_none());
        let mut overlay = record.overlay.unwrap();
        assert_eq!(format!("{:?}", overlay.state), "Staged");
        crate::runtime::apply_overlay(&mut overlay).unwrap();
        assert_eq!(std::fs::read(target.join("base.txt")).unwrap(), b"changed");
        assert_eq!(std::fs::read(target.join("new.txt")).unwrap(), b"new");
    }

    #[test]
    fn builder_injects_runtime_and_network_markers() {
        let pvisor = PVisor::builder().build();
        let mut spec = RunSpec::process("run-implant", "agent", "echo");
        spec.capabilities.network = NetworkCapability::Deny;
        let plan = pvisor.plan_for(&spec);
        assert_eq!(
            plan.env
                .get("PERSISTING_PVISOR_RUNTIME")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            plan.env
                .get("PERSISTING_NETWORK_POLICY")
                .map(String::as_str),
            Some("deny")
        );
        assert!(plan.notes.iter().any(|note| note.contains("network")));
    }
}
