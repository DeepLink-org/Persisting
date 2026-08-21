//! pVisor — foreground Agent Run manager and portable execution runtime.
//!
//! Callers configure a [`PVisor`] and invoke [`PVisor::run`]. There is no
//! separate control plane: CLI / pPilot talk to this API directly.

use crate::config::{GatewayDriverConfig, NetworkDriverConfig, PVisorConfig};
use crate::event::{EventSink, NoopEventSink, RunEventPublisher};
use crate::executor::{AttemptContext, RunExecutor};
use crate::process::ProcessExecutor;
use crate::runtime::{
    AttemptTeardown, ImplantPlan, OverlayHint, RuntimeCapabilities, RuntimeSupervisor,
    RuntimeSupervisorBuilder,
};
use crate::util::unix_now_ms;
use crate::TrajectoryEventSink;
use crate::{AgentCtlServer, AGENTCTL_VERSION};
use persisting_agentctl::ControlController;
use persisting_agentctl::{
    AttemptId, AttemptInfo, CapabilityDimension, CapabilityEnforcementEvidence, EnforcementLevel,
    ExecutorDescriptor, IsolationKind, NetworkCapability, PolicyMode, RunFailure, RunFailureKind,
    RunInvocation, RunResult, RunSpec, RunState, RunStatus, RUNTIME_SCHEMA_VERSION,
};
use persisting_events::{ChronicleControl, ChronicleServeProcessClient, EventRecord};
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
    #[error("AgentCtl setup failed: {0}")]
    AgentCtl(#[source] anyhow::Error),
    #[error("no executor supports this invocation")]
    UnsupportedInvocation,
    #[error(
        "executor `{executor}` lacks enforced evidence for requested capability dimensions: {dimensions}"
    )]
    UnsupportedPolicy {
        executor: String,
        dimensions: String,
    },
    #[error("event sink rejected run creation: {0}")]
    EventSink(#[source] anyhow::Error),
    #[error("durable Attempt registration failed: {0}")]
    AttemptRegistry(#[source] anyhow::Error),
    #[error("run task failed to join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type RunEventStream = broadcast::Receiver<EventRecord>;

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
    run_id: persisting_agentctl::RunId,
    attempt_id: AttemptId,
    status: watch::Receiver<RunStatus>,
    cancellation: CancellationToken,
    events: RunEventPublisher,
    agentctl: crate::AgentCtlControl,
    checkpoint_record: Option<crate::runtime::RunRecord>,
    join: JoinHandle<RunResult>,
}

impl RunHandle {
    pub fn run_id(&self) -> &persisting_agentctl::RunId {
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

    /// Run-scoped cooperative AgentCtl desired-state and observation surface.
    pub fn agentctl(&self) -> crate::AgentCtlControl {
        self.agentctl.clone()
    }

    /// Cooperatively quiesce every connected AgentCtl client and snapshot the upper.
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
        let checkpoint = self
            .agentctl
            .begin_checkpoint(checkpoint_id.to_owned(), Some(deadline))?;
        loop {
            if let Some(captured) = checkpoint.try_capture(|| {
                crate::checkpoint::create_agent_quiesced_checkpoint(record, checkpoint_id)
            })? {
                return Ok(captured);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
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

/// Builder for a configured [`PVisor`].
#[derive(Clone, Default)]
pub struct PVisorBuilder {
    runtime: RuntimeSupervisorBuilder,
    event_sink: Option<Arc<dyn EventSink>>,
    executors: Option<Vec<Arc<dyn RunExecutor>>>,
    chronicle_control: Option<Arc<dyn ChronicleControl>>,
    pchronicle_binary: std::path::PathBuf,
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
            .field(
                "chronicle_control",
                &self
                    .chronicle_control
                    .as_ref()
                    .map(|_| "<ChronicleControl>"),
            )
            .field("pchronicle_binary", &self.pchronicle_binary)
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
        self.runtime = self.runtime.network(config.network);
        self
    }

    /// Enable pVisor's built-in Agent protocol Gateway driver.
    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.runtime = self.runtime.gateway(gateway);
        self
    }

    /// Configure the Attempt network policy and interception-driver selection.
    pub fn network(mut self, network: NetworkDriverConfig) -> Self {
        self.runtime = self.runtime.network(network);
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

    /// Set durable Run storage independently of the optional Gateway.
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

    /// Inject the lightweight pChronicle control-plane port used for durable
    /// Attempt registration. Storage engines remain outside the pVisor process.
    pub fn chronicle_control(mut self, control: Arc<dyn ChronicleControl>) -> Self {
        self.chronicle_control = Some(control);
        self
    }

    /// Select the sidecar executable used when a Run requests durable Attempt
    /// registration without an injected control connection.
    pub fn pchronicle_binary(mut self, binary: impl Into<std::path::PathBuf>) -> Self {
        self.pchronicle_binary = binary.into();
        self
    }

    pub fn executors(mut self, executors: Vec<Arc<dyn RunExecutor>>) -> Self {
        self.executors = Some(executors);
        self
    }

    pub fn build(self) -> PVisor {
        PVisor {
            executors: Arc::new(self.executors.unwrap_or_else(|| {
                vec![Arc::new(ProcessExecutor::default()) as Arc<dyn RunExecutor>]
            })),
            event_sink: self
                .event_sink
                .unwrap_or_else(|| Arc::new(NoopEventSink) as Arc<dyn EventSink>),
            runtime: self.runtime.build(),
            chronicle_control: self.chronicle_control,
            pchronicle_binary: if self.pchronicle_binary.as_os_str().is_empty() {
                "pchronicle".into()
            } else {
                self.pchronicle_binary
            },
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
    chronicle_control: Option<Arc<dyn ChronicleControl>>,
    pchronicle_binary: std::path::PathBuf,
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
    pub async fn run(&self, mut spec: RunSpec) -> Result<RunHandle, PVisorError> {
        validate_spec(&spec)?;
        let executor = self
            .executors
            .iter()
            .find(|executor| executor.supports(&spec.invocation))
            .cloned()
            .ok_or(PVisorError::UnsupportedInvocation)?;
        let mut descriptor = executor.descriptor();
        let vm_executor = descriptor.kind == persisting_agentctl::ExecutorKind::VirtualMachine;
        let vm_network_executor = vm_executor && executor.supports_vm_network_attachment();
        if self.runtime.vm_network_is_requested()
            && descriptor.isolation == persisting_agentctl::IsolationKind::VirtualMachine
            && !vm_network_executor
        {
            return Err(PVisorError::InvalidSpec(format!(
                "executor `{}` reports virtual-machine isolation but does not support pVisor VM network attachments",
                descriptor.name
            )));
        }
        self.runtime.apply_network_capability(&mut spec);
        let capability_enforcement = effective_capability_enforcement(
            &descriptor,
            &spec,
            self.runtime.proxy_network_is_configured(),
            vm_network_executor && self.runtime.vm_network_is_enforcing(),
        );
        if spec.runtime.policy_mode == PolicyMode::Enforce {
            let missing = capability_enforcement
                .missing_dimensions(&spec.capabilities, &spec.runtime.resource_limits);
            if !missing.is_empty() {
                return Err(PVisorError::UnsupportedPolicy {
                    executor: descriptor.name,
                    dimensions: missing
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }
        // Persist the effective, Run-specific evidence in Attempt status and
        // Run Bundle descriptors, including enforcement supplied by drivers.
        descriptor.capability_enforcement = capability_enforcement.clone();
        spec.metadata.insert(
            "pvisor.executor".into(),
            serde_json::to_value(&descriptor).map_err(|error| {
                PVisorError::InvalidSpec(format!("serialize executor descriptor: {error}"))
            })?,
        );
        spec.metadata.insert(
            "pvisor.capability_enforcement".into(),
            serde_json::to_value(&capability_enforcement).map_err(|error| {
                PVisorError::InvalidSpec(format!("serialize capability enforcement: {error}"))
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
        let mut session = self
            .runtime
            .prepare(
                &mut spec,
                &supervisor.initial_limits,
                vm_network_executor,
                &attempt_id,
            )
            .map_err(PVisorError::Prepare)?;
        let attachments = session
            .as_ref()
            .map(|session| session.attachments())
            .unwrap_or_default();
        let checkpoint_record = session
            .as_ref()
            .and_then(|session| session.checkpoint_record());
        let safe_profile_requested = spec
            .metadata
            .get("pvisor.safe")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let agentctl_server = match AgentCtlServer::start(&spec.run_id, &attempt_id) {
            Ok(server) => server,
            Err(error) => {
                if let Some(session) = session.take() {
                    let snapshot = empty_agentctl_snapshot(&spec.run_id, &attempt_id);
                    if let Err(cleanup_error) = session.abort_startup(
                        &attempt_id,
                        spec.lease_epoch,
                        snapshot,
                        safe_profile_requested,
                        format!("AgentCtl setup failed: {error:#}"),
                    ) {
                        tracing::warn!(%cleanup_error, "persist pVisor startup failure");
                    }
                }
                return Err(PVisorError::AgentCtl(error));
            }
        };
        let agentctl = agentctl_server.control();
        let bundle_agentctl = agentctl.clone();
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.env.extend(agentctl_server.environment());
        spec.metadata.insert(
            "pvisor.agentctl".into(),
            json!({
                "version": AGENTCTL_VERSION,
                "transport": "unix",
                "endpoint": agentctl.endpoint(),
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
        if let Err(error) = events
            .publish(
                "run.created",
                "runtime",
                json!({
                    "agent": spec.agent,
                    "task_id": spec.task_id,
                    "executor": descriptor,
                    "policy_mode": spec.runtime.policy_mode,
                    "capture_session": session.as_ref().map(|session| session.root_session()),
                    "agentctl_version": AGENTCTL_VERSION,
                }),
            )
            .await
        {
            if let Some(session) = session.take() {
                if let Err(cleanup_error) = session.abort_startup(
                    &attempt_id,
                    spec.lease_epoch,
                    agentctl.snapshot(),
                    safe_profile_requested,
                    format!("event sink rejected run creation: {error:#}"),
                ) {
                    tracing::warn!(%cleanup_error, "persist pVisor startup failure");
                }
            }
            return Err(PVisorError::EventSink(error));
        }

        let attempt_ttl_ms = spec
            .supervisor
            .as_ref()
            .map(|bootstrap| bootstrap.attempt_ttl_ms.max(1_000))
            .unwrap_or(15_000);
        let attempt_registry: Option<Arc<dyn ChronicleControl>> = match spec
            .supervisor
            .as_ref()
            .and_then(|bootstrap| bootstrap.attempt_registry_uri.as_deref())
        {
            Some(root) => {
                let registry = match self
                    .chronicle_control
                    .as_ref()
                    .filter(|control| control.root_uri() == root)
                {
                    Some(control) => Arc::clone(control),
                    None => Arc::new(
                        ChronicleServeProcessClient::spawn(&self.pchronicle_binary, root)
                            .await
                            .map_err(PVisorError::AttemptRegistry)?,
                    ) as Arc<dyn ChronicleControl>,
                };
                let registered = registry
                    .publish_attempt_active(
                        run_id.as_str(),
                        attempt_id.as_str(),
                        spec.lease_epoch,
                        attempt_ttl_ms,
                    )
                    .await
                    .map_err(PVisorError::AttemptRegistry)?;
                if !registered {
                    return Err(PVisorError::AttemptRegistry(anyhow::anyhow!(
                        "Run {} lease epoch {} was fenced before execution",
                        run_id,
                        spec.lease_epoch
                    )));
                }
                Some(registry)
            }
            None => None,
        };
        let attempt_heartbeat_stop = CancellationToken::new();
        if let Some(registry) = attempt_registry.as_ref().map(Arc::clone) {
            let heartbeat_run_id = run_id.to_string();
            let heartbeat_attempt_id = attempt_id.to_string();
            let heartbeat_epoch = spec.lease_epoch;
            let heartbeat_stop = attempt_heartbeat_stop.clone();
            let heartbeat_cancel = cancellation.clone();
            tokio::spawn(async move {
                let period_ms = (attempt_ttl_ms / 3).max(250);
                let mut interval =
                    tokio::time::interval(std::time::Duration::from_millis(period_ms));
                let mut last_success = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        _ = heartbeat_stop.cancelled() => break,
                        _ = interval.tick() => {
                            match registry
                                .heartbeat_attempt(
                                    &heartbeat_run_id,
                                    &heartbeat_attempt_id,
                                    heartbeat_epoch,
                                    attempt_ttl_ms,
                                )
                                .await
                            {
                                Ok(true) => last_success = tokio::time::Instant::now(),
                                Ok(false) => {
                                    tracing::warn!(
                                        run_id = %heartbeat_run_id,
                                        attempt_id = %heartbeat_attempt_id,
                                        "durable Attempt was fenced; cancelling workload"
                                    );
                                    heartbeat_cancel.cancel();
                                    break;
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        run_id = %heartbeat_run_id,
                                        attempt_id = %heartbeat_attempt_id,
                                        %error,
                                        "durable Attempt heartbeat failed"
                                    );
                                    if last_success.elapsed()
                                        >= std::time::Duration::from_millis(attempt_ttl_ms)
                                    {
                                        heartbeat_cancel.cancel();
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        let context = AttemptContext::new(
            Arc::new(spec),
            attempt_id.clone(),
            cancellation.clone(),
            status_tx,
            events.clone(),
            agentctl.clone(),
            attachments,
        );
        let supervisor_warning = supervisor.warning;
        let supervisor_session = supervisor.session;
        let join = tokio::spawn(async move {
            // Keep the Run-scoped endpoint alive until executor finalization finishes.
            let _agentctl_server = agentctl_server;
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
            if let Some(teardown) = teardown.as_mut() {
                let bundle_result = crate::RunBundle::capture(
                    teardown.run_record(),
                    &result,
                    bundle_agentctl.snapshot(),
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
                        &bundle_agentctl,
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
                        &bundle_agentctl,
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
                                &bundle_agentctl,
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
            if let Some(registry) = attempt_registry {
                match serde_json::to_value(&result) {
                    Ok(value) => match registry
                        .publish_attempt_terminal(
                            result.run_id.as_str(),
                            result.attempt_id.as_str(),
                            result.lease_epoch,
                            value,
                        )
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => result
                            .warnings
                            .push("durable Attempt terminal result was fenced".into()),
                        Err(error) => result.warnings.push(format!(
                            "publish durable Attempt terminal result failed: {error:#}"
                        )),
                    },
                    Err(error) => result.warnings.push(format!(
                        "encode durable Attempt terminal result failed: {error}"
                    )),
                }
            }
            attempt_heartbeat_stop.cancel();
            result
        });

        Ok(RunHandle {
            run_id,
            attempt_id,
            status: status_rx,
            cancellation,
            events,
            agentctl,
            checkpoint_record,
            join,
        })
    }
}

fn empty_agentctl_snapshot(
    run_id: &persisting_agentctl::RunId,
    attempt_id: &AttemptId,
) -> crate::AgentCtlSnapshot {
    crate::AgentCtlSnapshot {
        run_id: run_id.as_str().to_owned(),
        attempt_id: attempt_id.as_str().to_owned(),
        directive: crate::AgentDirective::Continue,
        clients: Vec::new(),
    }
}

fn effective_capability_enforcement(
    descriptor: &ExecutorDescriptor,
    spec: &RunSpec,
    proxy_network_configured: bool,
    vm_network_enforcing: bool,
) -> CapabilityEnforcementEvidence {
    let mut evidence = descriptor.capability_enforcement.clone();
    if proxy_network_configured {
        evidence.record(
            CapabilityDimension::Network,
            EnforcementLevel::Cooperative,
            "explicit-proxy-environment",
        );
    }
    if matches!(spec.capabilities.network, NetworkCapability::Deny) {
        match descriptor.isolation {
            IsolationKind::RootlessProcess => evidence.record(
                CapabilityDimension::Network,
                EnforcementLevel::Enforced,
                "linux-network-namespace",
            ),
            IsolationKind::SandboxedProcess => evidence.record(
                CapabilityDimension::Network,
                EnforcementLevel::Enforced,
                "macos-seatbelt-network-deny",
            ),
            _ => {}
        }
    }
    if vm_network_enforcing {
        evidence.record(
            CapabilityDimension::Network,
            EnforcementLevel::Enforced,
            "vm-smoltcp-network-boundary",
        );
    }
    evidence
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
    agentctl: &crate::AgentCtlControl,
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
        agentctl.snapshot(),
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
    let persisting_agentctl::RunInvocation::Process(process) = &spec.invocation;
    if process.program.trim().is_empty() {
        return Err(PVisorError::InvalidSpec(
            "process program must not be empty".into(),
        ));
    }
    if process.stdin == persisting_agentctl::StdioMode::Capture {
        return Err(PVisorError::InvalidSpec(
            "captured stdin is not supported in pVisor v1".into(),
        ));
    }
    if spec.runtime.max_output_bytes == 0 {
        return Err(PVisorError::InvalidSpec(
            "runtime.max_output_bytes must be greater than zero".into(),
        ));
    }
    let limits = &spec.runtime.resource_limits;
    if [
        limits.memory_bytes,
        limits.processes,
        limits.cpu_time_ms,
        limits.open_files,
        limits.file_size_bytes,
    ]
    .into_iter()
    .flatten()
    .any(|value| value == 0)
    {
        return Err(PVisorError::InvalidSpec(
            "runtime resource limits must be greater than zero when configured".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventSink, MemoryEventSink};
    use async_trait::async_trait;
    use persisting_agentctl::SupervisorBootstrap;
    use persisting_agentctl::{NetworkCapability, RunFailureKind, RunInvocation, StdioMode};
    use persisting_events::{AttemptRecordState, MemoryChronicleControl};
    use std::sync::Mutex;

    #[derive(Default)]
    struct RejectCompletedSink {
        kinds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EventSink for RejectCompletedSink {
        async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
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
        async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
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
        async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
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

    struct RejectCreatedSink;

    #[async_trait]
    impl EventSink for RejectCreatedSink {
        async fn append(&self, event: &EventRecord) -> anyhow::Result<()> {
            if event.kind == "run.created" {
                anyhow::bail!("simulated creation rejection");
            }
            Ok(())
        }

        fn classify_append_error(&self, _error: &anyhow::Error) -> crate::EventAppendErrorKind {
            crate::EventAppendErrorKind::Rejected
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_creation_finalizes_prepared_run_storage() {
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder()
            .storage(&storage)
            .event_sink(Arc::new(RejectCreatedSink))
            .build();
        let spec = RunSpec::process("run-created-rejected", "test-agent", "/bin/true");

        let error = match runtime.run(spec).await {
            Ok(_) => panic!("run creation unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(matches!(error, PVisorError::EventSink(_)));

        let record = crate::runtime::RunRecord::read(&storage).unwrap();
        assert_eq!(record.state, "failed");
        assert!(record.finished_at_unix_ms.is_some());
        let bundle = crate::RunBundle::read(&storage).unwrap();
        assert_eq!(bundle.run.state, RunState::Failed);
        assert_eq!(
            bundle.run.failure.as_ref().map(|failure| failure.kind),
            Some(RunFailureKind::Infrastructure)
        );
        assert!(bundle
            .run
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("event sink rejected run creation"));
        assert!(!storage.join("control.sock").exists());
        let _lease = crate::runtime::RunLease::acquire(&storage).unwrap();
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

        let emitted = sink.events();
        assert!(emitted.iter().all(|event| {
            event.identity.event_id.is_some()
                && event.identity.run_id.as_deref() == Some("run-success")
                && event.identity.attempt_id.is_some()
                && event.identity.producer.as_deref() == Some("persisting-pvisor")
        }));
        let kinds: Vec<_> = emitted.into_iter().map(|event| event.kind).collect();
        assert_eq!(kinds.first().map(String::as_str), Some("run.created"));
        assert_eq!(kinds.last().map(String::as_str), Some("run.completed"));
        assert!(kinds.iter().any(|kind| kind == "run.state_changed"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn durable_attempt_registry_receives_terminal_run_result() {
        let dir = tempfile::tempdir().unwrap();
        let control = Arc::new(MemoryChronicleControl::new(
            dir.path().display().to_string(),
        ));
        let mut spec = RunSpec::process("run-durable-registry", "test-agent", "/bin/sh");
        spec.lease_epoch = 7;
        spec.supervisor = Some(SupervisorBootstrap {
            endpoint: "tcp://127.0.0.1:9".into(),
            token: "unavailable".into(),
            controller_epoch: 1,
            connect_timeout_ms: 25,
            attempt_registry_uri: Some(dir.path().display().to_string()),
            attempt_ttl_ms: 1_000,
        });
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "printf durable".into()];
        process.stdout = StdioMode::Capture;

        let result = PVisor::builder()
            .chronicle_control(control.clone())
            .build()
            .run(spec)
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert_eq!(result.state, RunState::Completed);
        let record = control
            .get_attempt("run-durable-registry")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.state, AttemptRecordState::Terminal);
        assert_eq!(record.lease_epoch, 7);
        let recovered: RunResult = serde_json::from_value(record.terminal_result.unwrap()).unwrap();
        assert_eq!(recovered.attempt_id, result.attempt_id);
        assert_eq!(recovered.state, RunState::Completed);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn process_receives_live_agentctl_endpoint() {
        let runtime = PVisor::new();
        let mut spec = RunSpec::process("run-agentctl", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec![
            "-c".into(),
            "test -S \"$PERSISTING_AGENTCTL_ENDPOINT\" && \
             test -n \"$PERSISTING_AGENTCTL_TOKEN\" && \
             test \"$PERSISTING_AGENTCTL_VERSION\" = 1 && \
             test \"$PERSISTING_AGENTCTL_TRANSPORT\" = unix"
                .into(),
        ];

        let handle = runtime.run(spec).await.unwrap();
        assert_eq!(handle.agentctl().snapshot().run_id, "run-agentctl");
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

    #[cfg(unix)]
    #[tokio::test]
    async fn durable_run_metadata_records_environment_keys_without_secret_values() {
        const SECRET: &str = "pvisor-secret-value-must-not-be-persisted";
        let temporary = tempfile::tempdir().unwrap();
        let storage = temporary.path().join("storage");
        let runtime = PVisor::builder().storage(&storage).build();
        let mut spec = RunSpec::process("run-secret-projection", "test-agent", "/bin/sh");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.args = vec!["-c".into(), "exit 0".into()];
        process.inherit_env = false;
        process
            .env
            .insert("PRIVATE_API_TOKEN".into(), SECRET.into());

        let result = runtime.run(spec).await.unwrap().wait().await.unwrap();
        assert_eq!(result.state, RunState::Completed);

        let bundle_raw = std::fs::read_to_string(storage.join(crate::RUN_BUNDLE_FILENAME)).unwrap();
        let record_raw = std::fs::read_to_string(storage.join("run.json")).unwrap();
        assert!(!bundle_raw.contains(SECRET));
        assert!(!record_raw.contains(SECRET));
        let bundle = crate::RunBundle::read(&storage).unwrap();
        assert!(!bundle.environment.inherits_host);
        assert!(bundle
            .environment
            .projected_keys
            .iter()
            .any(|key| key == "PRIVATE_API_TOKEN"));
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
        assert!(matches!(
            error,
            PVisorError::UnsupportedPolicy { dimensions, .. }
                if dimensions == "network, subprocess"
        ));
    }

    #[test]
    fn ambient_network_requires_an_enforced_boundary() {
        let spec = RunSpec::process("run-ambient", "test-agent", "echo");
        assert_eq!(
            persisting_agentctl::requested_enforcement_dimensions(
                &spec.capabilities,
                &spec.runtime.resource_limits,
            ),
            vec![
                CapabilityDimension::Network,
                CapabilityDimension::Subprocess
            ]
        );
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
        assert!(matches!(
            error,
            PVisorError::UnsupportedPolicy { dimensions, .. }
                if dimensions == "network, subprocess"
        ));
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
