//! Attempt-scoped Gateway + OverlayFS session owned by pVisor.

use super::implant::{ImplantPlan, OverlayHint};
use super::overlay::{
    OverlayMount, OverlayRecord, apply_overlay, discard_overlay, hint_from_record,
    lower_stack_from_config, mount_overlay_record, prepare_overlay_record_mountless,
    resolve_overlay_workspace, stage_overlay_record,
};
use super::registry::{EnvironmentProjection, RunControlServer, RunLease, RunLineage, RunRecord};
use crate::TrajectoryEventSink;
use anyhow::Context as _;
use persisting_agentctl::ControlController;
use persisting_agentctl::{NetworkCapability, ProcessInvocation, RunInvocation, RunSpec, RunState};
use persisting_gateway::config::ProxyConfig;
use persisting_gateway::injection::{
    client_gateway_config_args, proxy_environment_with_local_auth,
};
use persisting_gateway::lifecycle::{
    CaptureMode, append_lifecycle, root_session_route, session_ended_record, session_started_record,
};
use persisting_gateway::runtime::in_process::{InProcessCapture, InProcessRuntime};
use persisting_gateway::runtime::run_config::snapshot_proxy_config;
use persisting_gateway::runtime::run_env::write_run_session;
use persisting_gateway::sink::SeqOnlySink;
use persisting_overlaynet::{
    BandwidthRegistry, EgressContext, EgressRuntime, InterceptionMetrics, NetworkConfig,
    NetworkPolicy,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Live controls for one Attempt: capture proxy + optional overlay mount.
pub(crate) struct AttemptSession {
    root_session: String,
    agent_id: String,
    /// Staging record retained after unmount (for apply / discard).
    overlay_record: Option<OverlayRecord>,
    gateway: Option<InProcessCapture>,
    vm_network: Option<Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>>,
    network_metrics: Option<InterceptionMetrics>,
    overlay: Option<OverlayMount>,
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    started_at: Instant,
    run_record: RunRecord,
    _control: Option<RunControlServer>,
    _lease: RunLease,
}

impl AttemptSession {
    pub(crate) fn root_session(&self) -> &str {
        &self.root_session
    }

    pub(crate) fn attachments(&self) -> crate::executor::AttemptAttachments {
        crate::executor::AttemptAttachments {
            vm_network: self.vm_network.clone(),
        }
    }
    pub(crate) fn checkpoint_record(&self) -> Option<RunRecord> {
        self.overlay_record
            .as_ref()
            .map(|_| self.run_record.clone())
    }

    pub(crate) fn teardown(mut self, exit_code: Option<i32>) -> AttemptTeardown {
        let mut errors = Vec::new();
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        if let Some(sink) = &self.sink
            && let Err(err) = append_lifecycle(
                sink.as_ref(),
                &root_session_route(&self.root_session),
                &self.agent_id,
                session_ended_record(
                    Some(self.root_session.clone()),
                    Some(self.agent_id.clone()),
                    CaptureMode::Run,
                    "child_exit",
                    exit_code,
                    Some(duration_ms),
                ),
            )
        {
            errors.push(format!("append session.ended: {err:#}"));
        }

        let mut record = if let Some(mount) = self.overlay.take() {
            let fallback = self.overlay_record.take();
            match mount.unmount() {
                Ok(record) => Some(record),
                Err(err) => {
                    errors.push(format!("unmount OverlayFS: {err:#}"));
                    fallback
                }
            }
        } else {
            let mut record = self.overlay_record.take();
            if let Some(record) = record.as_mut()
                && let Err(err) = stage_overlay_record(record)
            {
                errors.push(format!("stage OverlayFS: {err:#}"));
            }
            record
        };

        if let Some(ref mut rec) = record {
            if rec.auto_discard {
                if let Err(err) = discard_overlay(rec) {
                    errors.push(format!("discard OverlayFS staging: {err:#}"));
                }
            } else if rec.auto_apply {
                if let Err(err) = apply_overlay(rec) {
                    errors.push(format!("apply OverlayFS staging: {err:#}"));
                } else {
                    tracing::info!(
                        id = %rec.id,
                        target = %rec.target.display(),
                        "overlay auto-applied onto target"
                    );
                }
            } else {
                tracing::info!(
                    id = %rec.id,
                    stage = %rec.stage_dir.display(),
                    target = %rec.target.display(),
                    "overlay staged — review then: \
                     `pvisor status {}` then `pvisor apply {}` or `pvisor drop {}`",
                    rec.id,
                    rec.id,
                    rec.id,
                );
            }
        }
        self.overlay_record = record;

        if let Some(metrics) = &self.network_metrics {
            self.run_record.network_interception_metrics = Some(metrics.snapshot());
        }
        if let Some(network) = self.vm_network.take() {
            match network.lock() {
                Ok(mut attachment) => {
                    if let Some(attachment) = attachment.take() {
                        match attachment.shutdown() {
                            Ok(snapshot) => {
                                self.run_record.network_interception_metrics = Some(snapshot)
                            }
                            Err(err) => errors.push(format!("shutdown VM OverlayNet: {err:#}")),
                        }
                    }
                }
                Err(_) => errors.push("shutdown VM OverlayNet: attachment lock poisoned".into()),
            }
        }
        if let Some(gateway) = self.gateway.take()
            && let Err(err) = gateway.shutdown()
        {
            errors.push(format!("shutdown Gateway: {err:#}"));
        }
        self.run_record.finished_at_unix_ms = Some(crate::util::unix_now_ms());
        self.run_record.overlay = self.overlay_record.clone();
        AttemptTeardown {
            run_record: self.run_record,
            errors,
        }
    }

    /// Finalize durable state when pVisor prepared the Attempt drivers but
    /// could not hand the Attempt to an executor.
    ///
    /// Preparation writes a live RunRecord and may start an overlay, Gateway,
    /// or VM network attachment.  A later AgentCtl or event-publisher failure
    /// must therefore take the same teardown path as an executed Attempt
    /// instead of leaving a stale `running` record behind.
    pub(crate) fn abort_startup(
        self,
        attempt_id: &persisting_agentctl::AttemptId,
        lease_epoch: u64,
        agentctl: crate::AgentCtlSnapshot,
        safe_profile_requested: bool,
        message: String,
    ) -> anyhow::Result<()> {
        let run_id = persisting_agentctl::RunId::new(self.run_record.run_id.clone());
        let started_at_unix_ms = self.run_record.started_at_unix_ms;
        let mut teardown = self.teardown(None);
        let mut warnings = Vec::new();
        if let Some(error) = teardown.error_message() {
            warnings.push(format!("attempt teardown after startup failure: {error}"));
        }
        let result = persisting_agentctl::RunResult {
            run_id,
            attempt_id: attempt_id.clone(),
            lease_epoch,
            state: RunState::Failed,
            started_at_unix_ms,
            finished_at_unix_ms: crate::util::unix_now_ms(),
            exit_code: None,
            failure: Some(persisting_agentctl::RunFailure {
                kind: persisting_agentctl::RunFailureKind::Infrastructure,
                message,
                retryable: true,
            }),
            output: Default::default(),
            value: None,
            metrics: Default::default(),
            artifacts: Vec::new(),
            event_stream_ref: None,
            warnings,
        };
        teardown.commit_state(RunState::Failed)?;
        crate::RunBundle::capture(
            teardown.run_record(),
            &result,
            agentctl,
            safe_profile_requested,
        )?
        .write(&teardown.run_record().stage_dir())?;
        Ok(())
    }
}

pub(crate) struct AttemptTeardown {
    run_record: RunRecord,
    errors: Vec<String>,
}

impl AttemptTeardown {
    pub(crate) fn run_record(&self) -> &RunRecord {
        &self.run_record
    }

    pub(crate) fn error_message(&self) -> Option<String> {
        (!self.errors.is_empty()).then(|| self.errors.join("; "))
    }

    pub(crate) fn commit_state(&mut self, state: RunState) -> anyhow::Result<()> {
        self.run_record.state = match state {
            RunState::Completed => "completed",
            RunState::Cancelled => "cancelled",
            RunState::Failed => "failed",
            _ => "terminated",
        }
        .into();
        self.run_record.write()
    }
}

pub(crate) struct AttemptPrepareOpts<'a> {
    pub config: &'a ProxyConfig,
    /// Durable pVisor Run storage and default OverlayFS stage.
    pub storage: &'a Path,
    /// Gateway capture and session configuration storage.
    pub capture_storage: &'a Path,
    pub sink: Option<Arc<dyn TrajectoryEventSink>>,
    pub stream_markdown: bool,
    /// Extra overlay hint from CLI (overrides paths when set).
    pub overlay_override: OverlayHint,
    pub controller: Arc<dyn ControlController>,
    pub gateway_enabled: bool,
    pub vm_network: bool,
    pub attempt_id: &'a str,
}

pub(crate) struct OverlayAttemptPrepareOpts<'a> {
    pub storage: &'a Path,
    pub overlay: OverlayHint,
    pub vm_network: Option<VmNetworkPrepareOpts>,
}

#[derive(Clone)]
pub(crate) struct VmNetworkPrepareOpts {
    pub network: NetworkConfig,
    pub controller: Arc<dyn ControlController>,
    pub attempt_id: String,
}

pub(crate) struct VmNetworkAttachment {
    guest_stream: std::os::unix::net::UnixStream,
    backend: persisting_overlaynet::vm::VmNetwork,
}

impl VmNetworkAttachment {
    pub(crate) fn guest_stream(&self) -> &std::os::unix::net::UnixStream {
        &self.guest_stream
    }

    /// Close the peer first so a backend blocked on socket I/O can observe EOF
    /// before we join its thread.
    pub(crate) fn shutdown(self) -> anyhow::Result<persisting_overlaynet::InterceptionSnapshot> {
        let Self {
            backend,
            guest_stream,
        } = self;
        drop(guest_stream);
        backend.shutdown()
    }
}

fn mark_vm_network(plan: &mut ImplantPlan) {
    plan.env
        .insert("PERSISTING_OVERLAYNET_DRIVER".into(), "vm-smoltcp".into());
    plan.env.insert(
        "PERSISTING_OVERLAYNET_STRENGTH".into(),
        "non-bypassable".into(),
    );
    plan.notes.push(
        "network interception: libkrun virtio-net → smoltcp (non-bypassable IPv4 TCP + DNS)".into(),
    );
}

struct PreparedVmNetwork {
    attachment: Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>,
    metrics: InterceptionMetrics,
    policy: serde_json::Value,
}

struct PreparedOverlay {
    mount: Option<OverlayMount>,
    hint: OverlayHint,
    record: Option<OverlayRecord>,
    lowers: Vec<std::path::PathBuf>,
}

/// Start pVisor's configured Gateway and OverlayFS drivers, then enrich `spec`.
pub(crate) fn prepare_attempt(
    spec: &mut RunSpec,
    opts: AttemptPrepareOpts<'_>,
) -> anyhow::Result<AttemptSession> {
    let config = opts.config.clone();
    spec.agent.name = config.agent_id.clone();
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());
    let capture_storage = opts
        .capture_storage
        .canonicalize()
        .unwrap_or_else(|_| opts.capture_storage.to_path_buf());

    let sink = opts
        .sink
        .unwrap_or_else(|| Arc::new(SeqOnlySink::new()) as Arc<dyn TrajectoryEventSink>);

    let network_metrics = InterceptionMetrics::default();
    let bandwidth_registry = BandwidthRegistry::default();
    let gateway = InProcessCapture::start_with_runtime(
        config.clone(),
        capture_storage.clone(),
        Arc::clone(&sink),
        opts.stream_markdown,
        InProcessRuntime {
            controller: Arc::clone(&opts.controller),
            interception_metrics: network_metrics.clone(),
            bandwidth_registry: bandwidth_registry.clone(),
            attempt_id: Some(opts.attempt_id.to_owned()),
        },
    )?;

    // A Run has one top-level identity across pVisor, Gateway and pChronicle.
    // Subagent sessions remain separate Storylines beneath this root.
    let root_session = spec.run_id.as_str().to_string();
    write_run_session(&capture_storage, &root_session)?;
    let config_snapshot = snapshot_proxy_config(&capture_storage, &root_session, &config)?;

    let mut overlay_cfg = config.overlay.clone();
    apply_overlay_override(&mut overlay_cfg, &opts.overlay_override);

    let prepared_overlay = prepare_overlay(
        &overlay_cfg,
        &storage,
        &root_session,
        uses_krun_executor(spec),
    )?;
    let PreparedOverlay {
        mount: overlay_mount,
        hint: overlay_hint,
        record: overlay_record,
        lowers: overlay_lowers,
    } = prepared_overlay;

    let RunInvocation::Process(process) = &spec.invocation;
    let command = std::iter::once(process.program.clone())
        .chain(process.args.iter().cloned())
        .collect::<Vec<_>>();
    let stage_dir = overlay_record
        .as_ref()
        .map(|record| record.stage_dir.clone())
        .unwrap_or_else(|| storage.clone());
    let lease = RunLease::acquire(&stage_dir)?;
    let vm_network = opts
        .vm_network
        .then(|| {
            start_vm_network(
                spec,
                VmNetworkPrepareOpts {
                    network: config.network.clone(),
                    controller: Arc::clone(&opts.controller),
                    attempt_id: opts.attempt_id.to_owned(),
                },
                Some((&gateway.listen, opts.gateway_enabled)),
                network_metrics.clone(),
                bandwidth_registry,
            )
        })
        .transpose()?;
    let mut run_record = RunRecord {
        schema_version: 1,
        run_id: spec.run_id.as_str().to_string(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: config.agent_id.clone(),
        pid: std::process::id(),
        command,
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        workspace: workspace_from_spec(spec),
        overlaynet_listen: Some(gateway.listen.clone()),
        network_interception: Some(if opts.vm_network {
            persisting_overlaynet::InterceptionProfile::vm_smoltcp()
        } else {
            persisting_overlaynet::InterceptionProfile::explicit_proxy()
        }),
        network_interception_metrics: None,
        gateway_listen: opts.gateway_enabled.then(|| gateway.listen.clone()),
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy: Some(serde_json::to_value(&config.network)?),
        environment: environment_from_spec(spec),
        resource_limits: spec.runtime.resource_limits.clone(),
        overlay: overlay_record.clone(),
        overlay_lowers,
        lineage: lineage_from_spec(spec),
        orchestration: orchestration_from_spec(spec),
    };
    run_record.write()?;
    let control = RunControlServer::start(&run_record)?;

    let RunInvocation::Process(ref process) = spec.invocation;
    let program = process.program.clone();
    append_lifecycle(
        sink.as_ref(),
        &root_session_route(&root_session),
        &config.agent_id,
        session_started_record(
            Some(root_session.clone()),
            Some(config.agent_id.clone()),
            CaptureMode::Run,
            Some(&gateway.listen),
            Some(program.as_str()),
        ),
    )?;

    let implant = enrich_with_session(
        spec,
        SessionImplantOpts {
            listen: &gateway.listen,
            root_session: &root_session,
            overlay: &overlay_hint,
            overlay_record: overlay_record.as_ref(),
            run_storage: &storage,
            capture_storage: &capture_storage,
            config_path: &config_snapshot,
            gateway_enabled: opts.gateway_enabled,
            local_gateway_auth: opts.gateway_enabled
                && config
                    .models
                    .iter()
                    .any(|route| route.api_key.is_some() || route.api_key_env.is_some()),
        },
    )?;
    run_record.environment.runtime_injected_keys = implant.env.keys().cloned().collect();
    run_record.write()?;
    if opts.vm_network && opts.gateway_enabled {
        rewrite_vm_gateway_implant(spec, &gateway.listen);
    }
    inject_krun_overlay_metadata(spec, &overlay_hint, overlay_record.as_ref());

    Ok(AttemptSession {
        root_session,
        agent_id: config.agent_id.clone(),
        overlay_record,
        gateway: Some(gateway),
        vm_network,
        network_metrics: Some(network_metrics),
        overlay: overlay_mount,
        sink: Some(sink),
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
}

/// Prepare a durable OverlayFS Run without enabling the optional Gateway.
pub(crate) fn prepare_overlay_attempt(
    spec: &mut RunSpec,
    opts: OverlayAttemptPrepareOpts<'_>,
) -> anyhow::Result<AttemptSession> {
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());
    let root_session = spec.run_id.as_str().to_string();
    let mut overlay_cfg = persisting_gateway::config::OverlayConfig::default();
    apply_overlay_override(&mut overlay_cfg, &opts.overlay);
    let prepared_overlay = prepare_overlay(
        &overlay_cfg,
        &storage,
        &root_session,
        uses_krun_executor(spec),
    )?;
    let PreparedOverlay {
        mount: overlay_mount,
        hint: overlay_hint,
        record: overlay_record,
        lowers: overlay_lowers,
    } = prepared_overlay;
    let overlay_record = overlay_record.ok_or_else(|| {
        anyhow::anyhow!("overlay preparation requested without a target or lower directory")
    })?;

    let RunInvocation::Process(process) = &spec.invocation;
    let command = std::iter::once(process.program.clone())
        .chain(process.args.iter().cloned())
        .collect::<Vec<_>>();
    let lease = RunLease::acquire(&overlay_record.stage_dir)?;
    let prepared_network = opts
        .vm_network
        .map(|network| prepare_vm_network(spec, network, None))
        .transpose()?;
    let vm_network = prepared_network
        .as_ref()
        .map(|network| Arc::clone(&network.attachment));
    let network_metrics = prepared_network
        .as_ref()
        .map(|network| network.metrics.clone());
    let network_policy = prepared_network.map(|network| network.policy);
    let mut run_record = RunRecord {
        schema_version: 1,
        run_id: spec.run_id.as_str().to_string(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: spec.agent.name.clone(),
        pid: std::process::id(),
        command,
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        workspace: workspace_from_spec(spec),
        overlaynet_listen: None,
        network_interception: vm_network
            .as_ref()
            .map(|_| persisting_overlaynet::InterceptionProfile::vm_smoltcp()),
        network_interception_metrics: None,
        gateway_listen: None,
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy,
        environment: environment_from_spec(spec),
        resource_limits: spec.runtime.resource_limits.clone(),
        overlay: Some(overlay_record.clone()),
        overlay_lowers,
        lineage: lineage_from_spec(spec),
        orchestration: orchestration_from_spec(spec),
    };
    run_record.write()?;
    let control = RunControlServer::start(&run_record)?;

    let mut plan = ImplantPlan {
        env: ImplantPlan::marker_env(),
        cwd: overlay_hint.merged_dir.clone(),
        overlay: overlay_hint,
        notes: vec![format!(
            "filesystem: overlay target={} staging={} (apply later unless auto_apply)",
            overlay_record.target.display(),
            overlay_record.stage_dir.display()
        )],
    };
    plan.env
        .insert("PERSISTING_RUN_ID".into(), spec.run_id.as_str().to_string());
    plan.env
        .insert("PERSISTING_AGENT".into(), spec.agent.name.clone());
    plan.env.insert(
        "PERSISTING_PVISOR_STORAGE".into(),
        storage.display().to_string(),
    );
    plan.env.insert(
        "PERSISTING_OVERLAY_TARGET".into(),
        overlay_record.target.display().to_string(),
    );
    plan.env.insert(
        "PERSISTING_OVERLAY_STAGE".into(),
        overlay_record.stage_dir.display().to_string(),
    );
    plan.env
        .insert("PERSISTING_OVERLAY_ID".into(), overlay_record.id.clone());
    if vm_network.is_some() {
        mark_vm_network(&mut plan);
    }
    match &overlay_record.upper {
        super::overlay::OverlayUpper::Directory { upper_dir, .. } => {
            plan.env.insert(
                "PERSISTING_OVERLAY_UPPER".into(),
                upper_dir.display().to_string(),
            );
        }
        super::overlay::OverlayUpper::Jujutsu {
            store_path,
            workspace,
            upper_dir,
        } => {
            plan.env.insert(
                "PERSISTING_OVERLAY_UPPER".into(),
                upper_dir.display().to_string(),
            );
            plan.env.insert(
                "PERSISTING_OVERLAY_JUJUTSU_STORE".into(),
                store_path.display().to_string(),
            );
            plan.env.insert(
                "PERSISTING_OVERLAY_JUJUTSU_WORKSPACE".into(),
                workspace.clone(),
            );
        }
    }
    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    run_record.environment.runtime_injected_keys = plan.env.keys().cloned().collect();
    run_record.write()?;
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
    inject_krun_overlay_metadata(spec, &plan.overlay, Some(&overlay_record));

    Ok(AttemptSession {
        root_session,
        agent_id: spec.agent.name.clone(),
        overlay_record: Some(overlay_record),
        gateway: None,
        vm_network,
        network_metrics,
        overlay: overlay_mount,
        sink: None,
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
}

/// Prepare metadata-only durable Run storage without Gateway or OverlayFS.
pub(crate) fn prepare_storage_attempt(
    spec: &mut RunSpec,
    storage: &Path,
    vm_network_opts: Option<VmNetworkPrepareOpts>,
) -> anyhow::Result<AttemptSession> {
    let storage = storage
        .canonicalize()
        .unwrap_or_else(|_| storage.to_path_buf());
    let root_session = spec.run_id.as_str().to_string();
    let RunInvocation::Process(process) = &spec.invocation;
    let command = std::iter::once(process.program.clone())
        .chain(process.args.iter().cloned())
        .collect::<Vec<_>>();
    let lease = RunLease::acquire(&storage)?;
    let prepared_network = vm_network_opts
        .map(|network| prepare_vm_network(spec, network, None))
        .transpose()?;
    let vm_network = prepared_network
        .as_ref()
        .map(|network| Arc::clone(&network.attachment));
    let network_metrics = prepared_network
        .as_ref()
        .map(|network| network.metrics.clone());
    let network_policy = prepared_network.map(|network| network.policy);
    let mut run_record = RunRecord {
        schema_version: 1,
        run_id: root_session.clone(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: spec.agent.name.clone(),
        pid: std::process::id(),
        command,
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        workspace: workspace_from_spec(spec),
        overlaynet_listen: None,
        network_interception: vm_network
            .as_ref()
            .map(|_| persisting_overlaynet::InterceptionProfile::vm_smoltcp()),
        network_interception_metrics: None,
        gateway_listen: None,
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy,
        environment: environment_from_spec(spec),
        resource_limits: spec.runtime.resource_limits.clone(),
        overlay: None,
        overlay_lowers: Vec::new(),
        lineage: lineage_from_spec(spec),
        orchestration: orchestration_from_spec(spec),
    };
    run_record.write()?;
    let control = RunControlServer::start(&run_record)?;

    let mut plan = ImplantPlan {
        env: ImplantPlan::marker_env(),
        cwd: None,
        overlay: OverlayHint::default(),
        notes: vec![format!("durable Run storage: {}", storage.display())],
    };
    plan.env
        .insert("PERSISTING_RUN_ID".into(), root_session.clone());
    plan.env
        .insert("PERSISTING_AGENT".into(), spec.agent.name.clone());
    plan.env.insert(
        "PERSISTING_PVISOR_STORAGE".into(),
        storage.display().to_string(),
    );
    if vm_network.is_some() {
        mark_vm_network(&mut plan);
    }
    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    run_record.environment.runtime_injected_keys = plan.env.keys().cloned().collect();
    run_record.write()?;
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());

    Ok(AttemptSession {
        root_session,
        agent_id: spec.agent.name.clone(),
        overlay_record: None,
        gateway: None,
        vm_network,
        network_metrics,
        overlay: None,
        sink: None,
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
}

fn start_vm_network(
    spec: &mut RunSpec,
    opts: VmNetworkPrepareOpts,
    gateway: Option<(&str, bool)>,
    metrics: InterceptionMetrics,
    bandwidth_registry: BandwidthRegistry,
) -> anyhow::Result<Arc<std::sync::Mutex<Option<VmNetworkAttachment>>>> {
    let policy = NetworkPolicy::compile(&opts.network)?;
    let egress =
        EgressRuntime::with_bandwidth_registry(policy, opts.controller, bandwidth_registry);
    let mut config = persisting_overlaynet::vm::VmNetworkConfig::new(
        egress,
        EgressContext {
            run_id: Some(spec.run_id.as_str().to_owned()),
            attempt_id: Some(opts.attempt_id),
            storyline_id: None,
        },
    );
    config.metrics = metrics;
    if let Some((listen, _)) = gateway.filter(|(_, enabled)| *enabled) {
        let host: std::net::SocketAddr = listen
            .strip_prefix("http://")
            .or_else(|| listen.strip_prefix("https://"))
            .unwrap_or(listen)
            .parse()
            .with_context(|| format!("parse Attempt Gateway listen address `{listen}`"))?;
        config.gateway = Some(persisting_overlaynet::vm::VmGatewayRoute {
            guest_port: host.port(),
            host,
        });
    }
    let (backend, guest_stream) = persisting_overlaynet::vm::VmNetwork::start(config)?;
    spec.metadata.insert(
        "pvisor.network.driver".into(),
        serde_json::Value::String("vm-smoltcp".into()),
    );
    spec.metadata.insert(
        "pvisor.network.guest_ipv4".into(),
        serde_json::Value::String(persisting_overlaynet::vm::GUEST_IPV4.to_string()),
    );
    Ok(Arc::new(std::sync::Mutex::new(Some(VmNetworkAttachment {
        guest_stream,
        backend,
    }))))
}

fn prepare_vm_network(
    spec: &mut RunSpec,
    opts: VmNetworkPrepareOpts,
    gateway: Option<(&str, bool)>,
) -> anyhow::Result<PreparedVmNetwork> {
    let metrics = InterceptionMetrics::default();
    let policy = serde_json::to_value(&opts.network)?;
    let attachment = start_vm_network(
        spec,
        opts,
        gateway,
        metrics.clone(),
        BandwidthRegistry::default(),
    )?;
    Ok(PreparedVmNetwork {
        attachment,
        metrics,
        policy,
    })
}

fn rewrite_vm_gateway_implant(spec: &mut RunSpec, listen: &str) {
    let listen = listen.trim_end_matches('/');
    let listen_authority = listen
        .strip_prefix("http://")
        .or_else(|| listen.strip_prefix("https://"))
        .unwrap_or(listen);
    let gateway_port = listen_authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .expect("Gateway listen address was validated before implant rewriting");
    let virtual_base = format!(
        "http://{}:{gateway_port}",
        persisting_overlaynet::vm::ROUTER_IPV4
    );
    let mut source_bases = vec![
        format!("http://{listen_authority}"),
        format!("https://{listen_authority}"),
    ];
    if listen_authority.starts_with("127.0.0.1:") {
        source_bases.push(format!("http://localhost:{gateway_port}"));
        source_bases.push(format!("https://localhost:{gateway_port}"));
    } else if listen_authority.starts_with("localhost:") {
        source_bases.push(format!("http://127.0.0.1:{gateway_port}"));
        source_bases.push(format!("https://127.0.0.1:{gateway_port}"));
    }
    let RunInvocation::Process(process) = &mut spec.invocation;
    for value in process.env.values_mut() {
        for source in &source_bases {
            if value.contains(source) {
                *value = value.replace(source, &virtual_base);
            }
        }
    }
    for argument in &mut process.args {
        for source in &source_bases {
            if argument.contains(source) {
                *argument = argument.replace(source, &virtual_base);
            }
        }
    }
    let no_proxy = format!(
        "127.0.0.1,localhost,{}",
        persisting_overlaynet::vm::ROUTER_IPV4
    );
    process.env.insert("NO_PROXY".into(), no_proxy.clone());
    process.env.insert("no_proxy".into(), no_proxy);
    process
        .env
        .insert("PERSISTING_GATEWAY_VIRTUAL_ADDR".into(), virtual_base);
}

fn lineage_from_spec(spec: &RunSpec) -> Option<RunLineage> {
    spec.metadata
        .get("pvisor.lineage")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn orchestration_from_spec(
    spec: &RunSpec,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    spec.metadata
        .iter()
        .filter(|(key, _)| key.starts_with("ppilot.") || key.starts_with("persisting.ppilot."))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn environment_from_spec(spec: &RunSpec) -> EnvironmentProjection {
    if let Some(value) = spec.metadata.get("pvisor.environment") {
        let inherits_host = value
            .get("inherits_host")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let projected_keys = value
            .get("projected_keys")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
        return EnvironmentProjection {
            inherits_host,
            projected_keys,
            runtime_injected_keys: Vec::new(),
        };
    }
    let RunInvocation::Process(process) = &spec.invocation;
    EnvironmentProjection {
        inherits_host: process.inherit_env,
        projected_keys: process.env.keys().cloned().collect(),
        runtime_injected_keys: Vec::new(),
    }
}

fn workspace_from_spec(spec: &RunSpec) -> Option<PathBuf> {
    spec.metadata
        .get("pvisor.workspace")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

fn executor_from_spec(spec: &RunSpec) -> Option<persisting_agentctl::ExecutorDescriptor> {
    spec.metadata
        .get("pvisor.executor")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn apply_overlay_override(
    overlay_cfg: &mut persisting_gateway::config::OverlayConfig,
    overlay_override: &OverlayHint,
) {
    overlay_cfg.backend = overlay_override.backend;
    overlay_cfg.auto_apply = overlay_override.auto_apply;
    overlay_cfg.auto_discard = overlay_override.auto_discard;
    overlay_cfg.protect_target = overlay_override.protect_target;
    if let Some(stage) = &overlay_override.stage_dir {
        overlay_cfg.stage_dir = Some(stage.display().to_string());
        overlay_cfg.enabled = true;
    }
    if let Some(merged) = &overlay_override.merged_dir {
        overlay_cfg.merged_dir = Some(merged.display().to_string());
        overlay_cfg.enabled = true;
    }
    if let Some(upper) = &overlay_override.upper_dir {
        overlay_cfg.upper_dir = Some(upper.display().to_string());
        overlay_cfg.backend = persisting_gateway::config::OverlayBackend::Directory;
        overlay_cfg.jujutsu_store_path = None;
        overlay_cfg.jujutsu_workspace = None;
    }
    if let Some(work) = &overlay_override.work_dir {
        overlay_cfg.work_dir = Some(work.display().to_string());
        overlay_cfg.backend = persisting_gateway::config::OverlayBackend::Directory;
        overlay_cfg.jujutsu_store_path = None;
        overlay_cfg.jujutsu_workspace = None;
    }
    if let Some(store) = &overlay_override.jujutsu_store_path {
        overlay_cfg.jujutsu_store_path = Some(store.display().to_string());
        overlay_cfg.backend = persisting_gateway::config::OverlayBackend::Jujutsu;
        overlay_cfg.upper_dir = None;
        overlay_cfg.work_dir = None;
    }
    if let Some(workspace) = &overlay_override.jujutsu_workspace {
        overlay_cfg.jujutsu_workspace = Some(workspace.clone());
    }
    if !overlay_override.lower_dirs.is_empty() {
        // The final lower is the base/apply target; preceding entries are
        // read-only compose layers ordered from highest to lowest priority.
        if overlay_cfg.target.is_none() {
            let (target, compose) = overlay_override
                .lower_dirs
                .split_last()
                .expect("non-empty lower stack");
            overlay_cfg.target = Some(target.display().to_string());
            overlay_cfg.lower_dirs = compose.iter().map(|p| p.display().to_string()).collect();
        } else {
            overlay_cfg.lower_dirs = overlay_override
                .lower_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect();
        }
        overlay_cfg.enabled = true;
    }
}

fn prepare_overlay(
    overlay_cfg: &persisting_gateway::config::OverlayConfig,
    storage: &Path,
    root_session: &str,
    mountless: bool,
) -> anyhow::Result<PreparedOverlay> {
    if !overlay_cfg.enabled && overlay_cfg.target.is_none() {
        return Ok(PreparedOverlay {
            mount: None,
            hint: OverlayHint::default(),
            record: None,
            lowers: Vec::new(),
        });
    }
    match resolve_overlay_workspace(overlay_cfg, storage, root_session)? {
        Some(record) => {
            if let Ok(existing) = RunRecord::read(&record.stage_dir) {
                anyhow::bail!(
                    "OverlayFS stage {} already belongs to Run {}; choose a unique stage_dir",
                    record.stage_dir.display(),
                    existing.run_id
                );
            }
            let lowers = lower_stack_from_config(overlay_cfg, storage, &record.target);
            let (mount, record) = if mountless {
                (None, prepare_overlay_record_mountless(&record, &lowers)?)
            } else {
                let mount = mount_overlay_record(&record, &lowers)?;
                let record = mount.record().clone();
                (Some(mount), record)
            };
            let mut hint = hint_from_record(&record, lowers.clone());
            if mountless {
                hint.merged_dir = None;
            }
            Ok(PreparedOverlay {
                mount,
                hint,
                record: Some(record),
                lowers,
            })
        }
        None => Ok(PreparedOverlay {
            mount: None,
            hint: OverlayHint::default(),
            record: None,
            lowers: Vec::new(),
        }),
    }
}

fn uses_krun_executor(spec: &RunSpec) -> bool {
    executor_from_spec(spec).is_some_and(|executor| executor.name.starts_with("libkrun-"))
}

fn inject_krun_overlay_metadata(
    spec: &mut RunSpec,
    hint: &OverlayHint,
    record: Option<&OverlayRecord>,
) {
    if !uses_krun_executor(spec) {
        return;
    }
    let Some(record) = record else {
        return;
    };
    let (upper, work) = match &record.upper {
        super::overlay::OverlayUpper::Directory {
            upper_dir,
            work_dir,
        } => (upper_dir.clone(), Some(work_dir.clone())),
        super::overlay::OverlayUpper::Jujutsu { upper_dir, .. } => (upper_dir.clone(), None),
    };
    spec.metadata.insert(
        "pvisor.vm.workspace_overlay".into(),
        serde_json::json!({
            "lowers": hint.lower_dirs,
            "upper": upper,
            "work": work,
            "preimages": record.stage_dir.join("preimages"),
            "excluded": record.excluded_paths,
        }),
    );
}

struct SessionImplantOpts<'a> {
    listen: &'a str,
    root_session: &'a str,
    overlay: &'a OverlayHint,
    overlay_record: Option<&'a OverlayRecord>,
    run_storage: &'a Path,
    capture_storage: &'a Path,
    config_path: &'a Path,
    gateway_enabled: bool,
    local_gateway_auth: bool,
}

fn enrich_with_session(
    spec: &mut RunSpec,
    opts: SessionImplantOpts<'_>,
) -> anyhow::Result<ImplantPlan> {
    let SessionImplantOpts {
        listen,
        root_session,
        overlay,
        overlay_record,
        run_storage,
        capture_storage,
        config_path,
        gateway_enabled,
        local_gateway_auth,
    } = opts;
    let mut plan = ImplantPlan {
        env: ImplantPlan::marker_env(),
        cwd: overlay.merged_dir.clone(),
        overlay: overlay.clone(),
        notes: Vec::new(),
    };

    plan.env
        .insert("PERSISTING_RUN_ID".into(), spec.run_id.as_str().to_string());
    plan.env
        .insert("PERSISTING_AGENT".into(), spec.agent.name.clone());
    plan.env.insert(
        "PERSISTING_CAPTURE_CONFIG".into(),
        config_path.display().to_string(),
    );
    plan.env.insert(
        "PERSISTING_CAPTURE_STORAGE".into(),
        capture_storage.display().to_string(),
    );
    plan.env.insert(
        "PERSISTING_PVISOR_STORAGE".into(),
        run_storage.display().to_string(),
    );
    plan.notes
        .push("network service: in-process HTTP proxy started".into());

    for (key, value) in proxy_environment_with_local_auth(listen, root_session, local_gateway_auth)
    {
        plan.env.insert(key, value);
    }
    plan.notes
        .push(format!("network service: proxy env → http://{listen}"));
    if uses_krun_executor(spec) {
        mark_vm_network(&mut plan);
    } else {
        plan.env.insert(
            "PERSISTING_OVERLAYNET_DRIVER".into(),
            "explicit-proxy".into(),
        );
        plan.env.insert(
            "PERSISTING_OVERLAYNET_STRENGTH".into(),
            "cooperative".into(),
        );
        plan.notes.push(
            "network interception: explicit proxy (cooperative; direct sockets remain ambient)"
                .into(),
        );
    }

    match &spec.capabilities.network {
        NetworkCapability::Ambient => {
            plan.env
                .insert("PERSISTING_NETWORK_POLICY".into(), "ambient".into());
            plan.notes
                .push("network: ambient (from capture config)".into());
        }
        NetworkCapability::Deny => {
            plan.env
                .insert("PERSISTING_NETWORK_POLICY".into(), "deny".into());
            plan.notes.push(if uses_krun_executor(spec) {
                "network: deny on the non-bypassable VM data plane".into()
            } else {
                "network: deny for traffic intercepted by the proxy".into()
            });
        }
        NetworkCapability::AllowList { hosts, rules } => {
            plan.env
                .insert("PERSISTING_NETWORK_POLICY".into(), "allowlist".into());
            plan.env
                .insert("PERSISTING_NETWORK_ALLOWLIST".into(), hosts.join(","));
            if let Ok(serialized) = serde_json::to_string(rules) {
                plan.env
                    .insert("PERSISTING_NETWORK_RULES".into(), serialized);
            }
            plan.notes.push(format!(
                "network: allowlist ({} legacy hosts, {} structured rules, applied to {} traffic)",
                hosts.len(),
                rules.len(),
                if uses_krun_executor(spec) {
                    "VM"
                } else {
                    "intercepted proxy"
                },
            ));
        }
        NetworkCapability::Policy {
            default_action,
            allow,
            deny,
            limits,
        } => {
            plan.env.insert(
                "PERSISTING_NETWORK_POLICY".into(),
                match default_action {
                    persisting_agentctl::NetworkDefaultAction::Allow => "default-allow",
                    persisting_agentctl::NetworkDefaultAction::Deny => "default-deny",
                }
                .into(),
            );
            for (key, value) in [
                ("PERSISTING_NETWORK_RULES", allow),
                ("PERSISTING_NETWORK_DENY", deny),
            ] {
                if let Ok(serialized) = serde_json::to_string(value) {
                    plan.env.insert(key.into(), serialized);
                }
            }
            if let Ok(serialized) = serde_json::to_string(limits) {
                plan.env
                    .insert("PERSISTING_NETWORK_LIMITS".into(), serialized);
            }
            plan.notes.push(format!(
                "network: policy ({} allow, {} deny, {} bandwidth limits, applied to {} traffic)",
                allow.len(),
                deny.len(),
                limits.len(),
                if uses_krun_executor(spec) {
                    "VM"
                } else {
                    "intercepted proxy"
                },
            ));
        }
    }

    if let Some(rec) = overlay_record {
        plan.env.insert(
            "PERSISTING_OVERLAY_TARGET".into(),
            rec.target.display().to_string(),
        );
        match &rec.upper {
            super::overlay::OverlayUpper::Directory { upper_dir, .. } => {
                plan.env.insert(
                    "PERSISTING_OVERLAY_UPPER".into(),
                    upper_dir.display().to_string(),
                );
            }
            super::overlay::OverlayUpper::Jujutsu {
                store_path,
                workspace,
                upper_dir,
            } => {
                plan.env.insert(
                    "PERSISTING_OVERLAY_UPPER".into(),
                    upper_dir.display().to_string(),
                );
                plan.env.insert(
                    "PERSISTING_OVERLAY_JUJUTSU_STORE".into(),
                    store_path.display().to_string(),
                );
                plan.env.insert(
                    "PERSISTING_OVERLAY_JUJUTSU_WORKSPACE".into(),
                    workspace.clone(),
                );
            }
        }
        plan.env.insert(
            "PERSISTING_OVERLAY_STAGE".into(),
            rec.stage_dir.display().to_string(),
        );
        plan.env
            .insert("PERSISTING_OVERLAY_ID".into(), rec.id.clone());
        plan.notes.push(format!(
            "filesystem: overlay target={} staging={} (apply later unless auto_apply)",
            rec.target.display(),
            rec.stage_dir.display()
        ));
    } else if overlay.merged_dir.is_some() {
        plan.notes
            .push("filesystem: embedded overlay merged root as cwd".into());
    } else {
        plan.notes.push("filesystem: host view (no overlay)".into());
    }

    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    if gateway_enabled {
        inject_gateway_args(process, listen);
    }
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
    Ok(plan)
}

fn inject_gateway_args(process: &mut ProcessInvocation, listen: &str) {
    let extra = client_gateway_config_args(&process.program, listen);
    if extra.is_empty() {
        return;
    }
    let mut args = extra;
    args.append(&mut process.args);
    process.args = args;
}

pub(crate) fn apply_implant(process: &mut ProcessInvocation, plan: &ImplantPlan) {
    for (key, value) in &plan.env {
        process
            .env
            .entry(key.clone())
            .or_insert_with(|| value.clone());
    }
    if process.cwd.is_none()
        && let Some(cwd) = &plan.cwd
    {
        process.cwd = Some(cwd.display().to_string());
    }
}

#[cfg(test)]
mod vm_network_tests {
    use super::rewrite_vm_gateway_implant;
    use persisting_agentctl::{RunInvocation, RunSpec};

    #[test]
    fn gateway_loopback_urls_and_embedded_arguments_are_rewritten() {
        let mut spec = RunSpec::process("run-1", "agent", "codex");
        let RunInvocation::Process(process) = &mut spec.invocation;
        process
            .env
            .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:19081/v1".into());
        process
            .args
            .push("openai_base_url=\"http://127.0.0.1:19081/v1\"".into());

        rewrite_vm_gateway_implant(&mut spec, "127.0.0.1:19081");

        let RunInvocation::Process(process) = &spec.invocation;
        assert_eq!(
            process.env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://192.0.2.1:19081/v1")
        );
        assert_eq!(
            process.args.last().map(String::as_str),
            Some("openai_base_url=\"http://192.0.2.1:19081/v1\"")
        );
        assert!(process.env["NO_PROXY"].contains("192.0.2.1"));
    }
}
