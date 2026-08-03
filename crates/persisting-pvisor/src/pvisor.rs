//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Callers configure a [`PVisor`] and invoke [`PVisor::run`]. There is no
//! separate control plane: CLI / pPilot talk to this API directly.

use crate::agent_abi::AgentAbiServer;
use crate::config::{GatewayDriverConfig, PVisorConfig};
use crate::event::{EventSink, NoopEventSink, RunEventPublisher};
use crate::executor::{AttemptContext, RunExecutor};
use crate::process::ProcessExecutor;
use crate::runtime::{
    AttemptTeardown, ImplantPlan, OverlayHint, RuntimeCapabilities, RuntimeSupervisor,
    RuntimeSupervisorBuilder,
};
use crate::util::unix_now_ms;
use crate::TrajectoryEventSink;
use persisting_control::ControlController;
use persisting_proto::{
    AgentCapability, AttemptId, AttemptInfo, EventEnvelope, PolicyMode, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunSpec, RunState, RunStatus, AGENT_ABI_VERSION,
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
    #[error("Agent ABI setup failed: {0}")]
    AgentAbi(#[source] anyhow::Error),
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

/// Cloneable, provider-independent cancellation capability for an in-flight Run.
#[derive(Clone)]
pub struct RunCancellation {
    token: CancellationToken,
}

impl RunCancellation {
    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// Handle for one in-flight Run: status, cancel, wait, event subscribe.
pub struct RunHandle {
    run_id: persisting_proto::RunId,
    attempt_id: AttemptId,
    status: watch::Receiver<RunStatus>,
    cancellation: CancellationToken,
    events: RunEventPublisher,
    agent_abi: crate::AgentAbiControl,
    checkpoint_record: Option<crate::runtime::RunRecord>,
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

    /// Run-scoped Agent ABI desired-state and observation surface.
    pub fn agent_abi(&self) -> crate::AgentAbiControl {
        self.agent_abi.clone()
    }

    /// Cooperatively quiesce every connected ABI client, verify that no
    /// external effects are open, and snapshot the live OverlayFS upper.
    pub async fn checkpoint(
        &self,
        checkpoint_id: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<crate::LogicalCheckpoint> {
        anyhow::ensure!(
            !checkpoint_id.trim().is_empty()
                && checkpoint_id != "."
                && checkpoint_id != ".."
                && !checkpoint_id.contains('/')
                && !checkpoint_id.contains('\\'),
            "checkpoint id must be one non-empty path-safe segment"
        );
        let record = self
            .checkpoint_record
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Run {} has no OverlayFS stage", self.run_id))?;
        let deadline = crate::unix_now_ms().saturating_add(timeout.as_millis() as u64);
        self.agent_abi
            .request_quiesce(checkpoint_id.to_owned(), Some(deadline));
        let outcome = async {
            loop {
                let snapshot = self.agent_abi.snapshot();
                if checkpoint_barrier_satisfied(&snapshot, checkpoint_id) {
                    return crate::checkpoint::create_agent_quiesced_checkpoint(
                        record,
                        checkpoint_id,
                    );
                }
                if crate::unix_now_ms() >= deadline {
                    anyhow::bail!(
                        "checkpoint {checkpoint_id} timed out waiting for all Agent ABI clients to quiesce with no open effects"
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
        .await;
        self.agent_abi.continue_execution();
        outcome
    }

    /// Cooperative cancel followed by executor-specific termination.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn cancellation(&self) -> RunCancellation {
        RunCancellation {
            token: self.cancellation.clone(),
        }
    }

    pub async fn wait(self) -> Result<RunResult, PVisorError> {
        Ok(self.join.await?)
    }
}

fn checkpoint_barrier_satisfied(snapshot: &crate::AgentAbiSnapshot, checkpoint_id: &str) -> bool {
    !snapshot.clients.is_empty()
        && snapshot.clients.iter().all(|client| {
            client
                .capabilities
                .contains(&AgentCapability::CheckpointQuiesce)
                && client.quiesced_checkpoint_id.as_deref() == Some(checkpoint_id)
        })
        && snapshot
            .effects
            .iter()
            .all(|effect| effect.completion.is_some())
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
        spec.metadata.insert(
            "pvisor.executor".into(),
            serde_json::to_value(&descriptor).map_err(|error| {
                PVisorError::InvalidSpec(format!("serialize executor descriptor: {error}"))
            })?,
        );
        let attempt_id = AttemptId::new(format!("attempt-{}", uuid::Uuid::new_v4()));
        let cancellation = CancellationToken::new();
        let supervisor = crate::supervisor::connect_optional(
            spec.supervisor.as_ref(),
            &spec.run_id,
            &attempt_id,
            spec.lease_epoch,
            cancellation.clone(),
        )
        .await;
        if let Some(connected) = supervisor.connected {
            spec.metadata.insert(
                "persisting.ppilot.supervisor.connected".into(),
                json!(connected),
            );
        }
        if let Some(controller_epoch) = supervisor.controller_epoch {
            spec.metadata.insert(
                "persisting.ppilot.supervisor.controller_epoch".into(),
                json!(controller_epoch),
            );
        }
        let session = self
            .runtime
            .prepare(&mut spec, &supervisor.initial_limits)
            .map_err(PVisorError::Prepare)?;
        let checkpoint_record = session
            .as_ref()
            .and_then(|session| session.checkpoint_record());
        let agent_abi_server =
            AgentAbiServer::start(&spec.run_id, &attempt_id).map_err(PVisorError::AgentAbi)?;
        let agent_abi = agent_abi_server.control();
        let bundle_agent_abi = agent_abi.clone();
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.env.extend(agent_abi_server.environment());
        spec.metadata.insert(
            "pvisor.agent_abi".into(),
            json!({
                "version": AGENT_ABI_VERSION,
                "transport": "unix",
                "endpoint": agent_abi.endpoint(),
            }),
        );

        let run_id = spec.run_id.clone();
        let now = unix_now_ms();
        let initial = RunStatus {
            run_id: run_id.clone(),
            state: RunState::Created,
            attempt: AttemptInfo {
                attempt_id: attempt_id.clone(),
                lease_epoch: spec.lease_epoch,
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
                    "agent_abi_version": AGENT_ABI_VERSION,
                }),
            )
            .await
            .map_err(PVisorError::EventSink)?;

        let context = AttemptContext::new(
            Arc::new(spec),
            attempt_id.clone(),
            cancellation.clone(),
            status_tx,
            events.clone(),
            agent_abi.clone(),
        );
        let supervisor_warning = supervisor.warning;
        let supervisor_session = supervisor.session;
        let join = tokio::spawn(async move {
            // Keep the Run-scoped endpoint alive until executor finalization finishes.
            let _agent_abi_server = agent_abi_server;
            let _supervisor_session = supervisor_session;
            let mut result = executor.execute(context.clone()).await;
            if let Some(warning) = supervisor_warning {
                result.warnings.push(warning);
            }
            // The owning pVisor, not a pluggable executor, is authoritative for
            // the scheduling generation attached to this Attempt.
            result.lease_epoch = context.spec().lease_epoch;
            let mut teardown = session.map(|session| session.teardown(result.exit_code));
            if let Some(error) = teardown
                .as_ref()
                .and_then(|teardown| teardown.error_message())
            {
                fail_finalization(&mut result, format!("attempt teardown failed: {error}"));
            }
            if let Some(teardown) = teardown.as_mut() {
                if let Err(error) = teardown.commit_state(result.state) {
                    fail_finalization(
                        &mut result,
                        format!("commit local Run record failed: {error:#}"),
                    );
                    if let Err(error) = teardown.commit_state(RunState::Failed) {
                        result.warnings.push(format!(
                            "commit failed Run record after finalization error: {error:#}"
                        ));
                    }
                }
            }
            let safe_profile_requested = context
                .spec()
                .metadata
                .get("pvisor.safe")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if let Some(teardown) = teardown.as_mut() {
                let bundle_result = crate::RunBundle::capture(
                    teardown.run_record(),
                    &result,
                    bundle_agent_abi.snapshot(),
                    safe_profile_requested,
                )
                .and_then(|bundle| bundle.write(&teardown.run_record().stage_dir()));
                if let Err(error) = bundle_result {
                    fail_finalization(
                        &mut result,
                        format!("write durable Run Bundle failed: {error:#}"),
                    );
                    persist_failed_local_state(
                        teardown,
                        &mut result,
                        &bundle_agent_abi,
                        safe_profile_requested,
                        false,
                    );
                }
            }
            let kind = match result.state {
                RunState::Completed => "run.completed",
                RunState::Cancelled => "run.cancelled",
                _ => "run.failed",
            };
            if let Err(error) = context
                .events()
                .publish(kind, "runtime", terminal_payload(&result))
                .await
            {
                let append_error_kind = context.events().classify_append_error(&error);
                fail_finalization(
                    &mut result,
                    format!("terminal event sink failed: {error:#}"),
                );
                if append_error_kind == crate::EventAppendErrorKind::Unknown {
                    result.warnings.push(
                        "terminal event append outcome is unknown; a replacement terminal event was suppressed"
                            .into(),
                    );
                }
                if let Some(teardown) = teardown.as_mut() {
                    persist_failed_local_state(
                        teardown,
                        &mut result,
                        &bundle_agent_abi,
                        safe_profile_requested,
                        true,
                    );
                }
                if append_error_kind == crate::EventAppendErrorKind::Rejected {
                    if let Err(error) = context
                        .events()
                        .publish("run.failed", "runtime", terminal_payload(&result))
                        .await
                    {
                        result.warnings.push(format!(
                            "publish finalization failure event failed: {error:#}"
                        ));
                        if let Some(teardown) = teardown.as_mut() {
                            persist_failed_local_state(
                                teardown,
                                &mut result,
                                &bundle_agent_abi,
                                safe_profile_requested,
                                true,
                            );
                        }
                    }
                }
            }
            context.finish(
                result.state,
                result.failure.as_ref().map(|f| f.message.clone()),
            );
            result
        });

        Ok(RunHandle {
            run_id,
            attempt_id,
            status: status_rx,
            cancellation,
            events,
            agent_abi,
            checkpoint_record,
            join,
        })
    }
}

fn terminal_payload(result: &RunResult) -> serde_json::Value {
    json!({
        "state": result.state,
        "lease_epoch": result.lease_epoch,
        "exit_code": result.exit_code,
        "failure": result.failure,
        "started_at_unix_ms": result.started_at_unix_ms,
        "finished_at_unix_ms": result.finished_at_unix_ms,
    })
}

fn persist_failed_local_state(
    teardown: &mut AttemptTeardown,
    result: &mut RunResult,
    agent_abi: &crate::AgentAbiControl,
    safe_profile_requested: bool,
    invalidate_stale_bundle: bool,
) {
    if let Err(error) = teardown.commit_state(RunState::Failed) {
        result
            .warnings
            .push(format!("commit failed Run record: {error:#}"));
    }
    let bundle_result = crate::RunBundle::capture(
        teardown.run_record(),
        result,
        agent_abi.snapshot(),
        safe_profile_requested,
    )
    .and_then(|bundle| bundle.write(&teardown.run_record().stage_dir()));
    if let Err(error) = bundle_result {
        result
            .warnings
            .push(format!("persist failed Run Bundle: {error:#}"));
        if invalidate_stale_bundle {
            if let Err(error) = crate::RunBundle::invalidate(&teardown.run_record().stage_dir()) {
                result
                    .warnings
                    .push(format!("invalidate stale Run Bundle: {error:#}"));
            }
        }
    }
}

fn fail_finalization(result: &mut RunResult, message: String) {
    result.warnings.push(message.clone());
    result.state = RunState::Failed;
    result.finished_at_unix_ms = unix_now_ms();
    result.failure = Some(RunFailure {
        kind: RunFailureKind::Infrastructure,
        message,
        retryable: true,
    });
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
    let run_id = spec.run_id.as_str().trim();
    if run_id == "." || run_id == ".." || run_id.contains('/') || run_id.contains('\\') {
        return Err(PVisorError::InvalidSpec(
            "run_id must be one non-empty path-safe segment".into(),
        ));
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
    use crate::{EventSink, MemoryEventSink};
    use async_trait::async_trait;
    use persisting_proto::{NetworkCapability, RunFailureKind, RunInvocation, StdioMode};
    use std::sync::Mutex;

    #[test]
    fn logical_checkpoint_barrier_requires_capable_quiesced_clients_and_closed_effects() {
        let mut snapshot = crate::AgentAbiSnapshot {
            run_id: "run".into(),
            attempt_id: "attempt".into(),
            directive_seq: 1,
            directive: persisting_proto::AgentDirective::Quiesce {
                checkpoint_id: "cp".into(),
                deadline_unix_ms: None,
            },
            clients: vec![],
            processes: vec![],
            effects: vec![],
        };
        assert!(!checkpoint_barrier_satisfied(&snapshot, "cp"));
        snapshot.clients.push(crate::AgentClientSnapshot {
            client_id: "agent".into(),
            agent_name: "agent".into(),
            role: persisting_proto::AgentClientRole::Agent,
            capabilities: vec![AgentCapability::CheckpointQuiesce],
            lifecycle: persisting_proto::AgentLifecycleState::Quiesced,
            last_heartbeat_unix_ms: Some(1),
            quiesced_checkpoint_id: Some("cp".into()),
        });
        assert!(checkpoint_barrier_satisfied(&snapshot, "cp"));
        snapshot.effects.push(crate::AgentEffectSnapshot {
            session_id: "session".into(),
            sequence: 1,
            begin: persisting_proto::AgentEffectBegin {
                effect_id: "effect".into(),
                kind: "write".into(),
                request_digest: "digest".into(),
                idempotency_key: None,
            },
            completion: None,
        });
        assert!(!checkpoint_barrier_satisfied(&snapshot, "cp"));
    }

    #[derive(Default)]
    struct RejectCompletedSink {
        kinds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventSink for RejectCompletedSink {
        async fn append(&self, event: &EventEnvelope) -> anyhow::Result<()> {
            if event.kind == "run.completed" {
                anyhow::bail!("simulated terminal commit failure");
            }
            self.kinds.lock().unwrap().push(event.kind.clone());
            Ok(())
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

    #[derive(Default)]
    struct CommitThenLoseAcknowledgementSink {
        kinds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventSink for CommitThenLoseAcknowledgementSink {
        async fn append(&self, event: &EventEnvelope) -> anyhow::Result<()> {
            self.kinds.lock().unwrap().push(event.kind.clone());
            if event.kind == "run.completed" {
                anyhow::bail!("simulated acknowledgement loss after commit");
            }
            Ok(())
        }
    }

    struct RejectAllTerminalEventsSink;

    #[async_trait]
    impl EventSink for RejectAllTerminalEventsSink {
        async fn append(&self, event: &EventEnvelope) -> anyhow::Result<()> {
            if matches!(
                event.kind.as_str(),
                "run.completed" | "run.cancelled" | "run.failed"
            ) {
                anyhow::bail!("simulated terminal rejection");
            }
            Ok(())
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

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
    async fn process_receives_live_agent_abi_endpoint() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-agent-abi", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "test -S \"$PERSISTING_AGENT_ABI_ENDPOINT\" && \
             test -n \"$PERSISTING_AGENT_ABI_TOKEN\" && \
             test \"$PERSISTING_AGENT_ABI_VERSION\" = 1 && \
             test \"$PERSISTING_AGENT_ABI_TRANSPORT\" = unix"
                .into(),
        ];

        let handle = runtime.run(spec).await.unwrap();
        assert_eq!(handle.agent_abi().snapshot().run_id, "run-agent-abi");
        let result = handle.wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_sink_failure_prevents_completed_result() {
        let sink = Arc::new(RejectCompletedSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-terminal-failure", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert_eq!(
            result.failure.as_ref().map(|failure| failure.kind),
            Some(RunFailureKind::Infrastructure)
        );
        assert!(result
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("terminal event sink failed"));
        let kinds = sink.kinds.lock().unwrap().clone();
        assert!(!kinds.iter().any(|kind| kind == "run.completed"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.failed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_terminal_append_does_not_publish_a_conflicting_terminal() {
        let sink = Arc::new(CommitThenLoseAcknowledgementSink::default());
        let runtime = PVisor::builder().event_sink(sink.clone()).build();
        let mut spec = RunSpec::process("run-terminal-unknown", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("outcome is unknown")));
        let terminal_kinds = sink
            .kinds
            .lock()
            .unwrap()
            .iter()
            .filter(|kind| {
                matches!(
                    kind.as_str(),
                    "run.completed" | "run.cancelled" | "run.failed"
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(terminal_kinds, vec!["run.completed"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacement_terminal_failure_is_reported_in_the_result() {
        let runtime = PVisor::builder()
            .event_sink(Arc::new(RejectAllTerminalEventsSink))
            .build();
        let mut spec = RunSpec::process("run-terminal-double-reject", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(result
            .warnings
            .iter()
            .any(|warning| warning.contains("publish finalization failure event failed")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bundle_failure_is_published_as_the_only_terminal_result() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let sink = Arc::new(MemoryEventSink::default());
        let runtime = PVisor::builder()
            .storage(&storage)
            .event_sink(sink.clone())
            .build();
        let mut spec = RunSpec::process("run-bundle-failure", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "mkdir -p \"$1\" && mkdir \"$1/run-bundle.json\"".into(),
            "sh".into(),
            storage.display().to_string(),
        ];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Failed);
        assert!(result
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("write durable Run Bundle failed"));
        let terminal_kinds = sink
            .events()
            .into_iter()
            .map(|event| event.kind)
            .filter(|kind| {
                matches!(
                    kind.as_str(),
                    "run.completed" | "run.cancelled" | "run.failed"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal_kinds, vec!["run.failed"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn storage_only_run_persists_a_bundle_without_network_drivers() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder().storage(&storage).build();
        let mut spec = RunSpec::process("run-storage-only", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);
        assert_eq!(
            crate::RunBundle::read(&storage).unwrap().run.state,
            RunState::Completed
        );
        assert_eq!(
            crate::runtime::RunRecord::read(&storage).unwrap().state,
            "completed"
        );
    }

    #[tokio::test]
    async fn run_id_must_be_a_capture_safe_path_segment() {
        for invalid in ["../escape", "nested/run", r"nested\run", ".", ".."] {
            let spec = RunSpec::process(invalid, "agent", "echo");
            let error = match PVisor::new().run(spec).await {
                Ok(_) => panic!("invalid run id was accepted: {invalid}"),
                Err(error) => error,
            };
            assert!(matches!(error, PVisorError::InvalidSpec(_)));
        }
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
        let plan = runtime.plan_for(&spec);
        assert_eq!(
            plan.env
                .get("PERSISTING_OVERLAYNET_DRIVER")
                .map(String::as_str),
            Some("explicit-proxy")
        );
        assert_eq!(
            plan.env
                .get("PERSISTING_OVERLAYNET_STRENGTH")
                .map(String::as_str),
            Some("cooperative")
        );
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
