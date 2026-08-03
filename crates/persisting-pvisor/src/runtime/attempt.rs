//! Attempt-scoped Gateway + OverlayFS session owned by pVisor.

use super::implant::{ImplantPlan, OverlayHint};
use super::overlay::{
    apply_overlay, discard_overlay, hint_from_record, lower_stack_from_config,
    mount_overlay_record, resolve_overlay_workspace, OverlayMount, OverlayRecord,
};
use super::registry::{RunControlServer, RunLease, RunLineage, RunRecord};
use crate::TrajectoryEventSink;
use persisting_control::ControlController;
use persisting_gateway::config::ProxyConfig;
use persisting_gateway::injection::{client_gateway_config_args, proxy_environment};
use persisting_gateway::lifecycle::{
    append_lifecycle, root_session_route, session_ended_record, session_started_record, CaptureMode,
};
use persisting_gateway::runtime::in_process::InProcessCapture;
use persisting_gateway::runtime::run_config::snapshot_proxy_config;
use persisting_gateway::runtime::run_env::write_run_session;
use persisting_gateway::sink::SeqOnlySink;
use persisting_overlaynet::policy::network_capability_from_config;
use persisting_proto::{NetworkCapability, ProcessInvocation, RunInvocation, RunSpec, RunState};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Live controls for one Attempt: capture proxy + optional overlay mount.
pub struct AttemptSession {
    pub root_session: String,
    pub agent_id: String,
    /// Staging record retained after unmount (for apply / discard).
    pub overlay_record: Option<OverlayRecord>,
    gateway: Option<InProcessCapture>,
    overlay: Option<OverlayMount>,
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    started_at: Instant,
    run_record: RunRecord,
    _control: Option<RunControlServer>,
    _lease: RunLease,
}

impl AttemptSession {
    pub(crate) fn checkpoint_record(&self) -> Option<RunRecord> {
        self.overlay_record
            .as_ref()
            .map(|_| self.run_record.clone())
    }

    pub(crate) fn teardown(mut self, exit_code: Option<i32>) -> AttemptTeardown {
        let mut errors = Vec::new();
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        if let Some(sink) = &self.sink {
            if let Err(err) = append_lifecycle(
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
            ) {
                errors.push(format!("append session.ended: {err:#}"));
            }
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
            self.overlay_record.take()
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

        if let Some(gateway) = self.gateway.take() {
            self.run_record.network_interception_metrics = Some(gateway.interception_snapshot());
            if let Err(err) = gateway.shutdown() {
                errors.push(format!("shutdown Gateway: {err:#}"));
            }
        }
        self.run_record.finished_at_unix_ms = Some(crate::util::unix_now_ms());
        self.run_record.overlay = self.overlay_record.clone();
        AttemptTeardown {
            run_record: self.run_record,
            errors,
        }
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

pub struct AttemptPrepareOpts<'a> {
    pub config: &'a ProxyConfig,
    pub storage: &'a Path,
    pub sink: Option<Arc<dyn TrajectoryEventSink>>,
    pub stream_markdown: bool,
    /// Extra overlay hint from CLI (overrides paths when set).
    pub overlay_override: OverlayHint,
    pub controller: Arc<dyn ControlController>,
    pub gateway_enabled: bool,
}

pub struct OverlayAttemptPrepareOpts<'a> {
    pub storage: &'a Path,
    pub overlay: OverlayHint,
}

struct PreparedOverlay {
    mount: Option<OverlayMount>,
    hint: OverlayHint,
    record: Option<OverlayRecord>,
    lowers: Vec<std::path::PathBuf>,
}

/// Start pVisor's configured Gateway and OverlayFS drivers, then enrich `spec`.
pub fn prepare_attempt(
    spec: &mut RunSpec,
    opts: AttemptPrepareOpts<'_>,
) -> anyhow::Result<AttemptSession> {
    let config = opts.config.clone();
    spec.agent.name = config.agent_id.clone();
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());

    spec.capabilities.network = network_capability_from_config(&config);

    let sink = opts
        .sink
        .unwrap_or_else(|| Arc::new(SeqOnlySink::new()) as Arc<dyn TrajectoryEventSink>);

    let gateway = InProcessCapture::start_with_control(
        config.clone(),
        storage.clone(),
        Arc::clone(&sink),
        opts.stream_markdown,
        opts.controller,
    )?;

    // A Run has one top-level identity across pVisor, Gateway and pChronicle.
    // Subagent sessions remain separate Storylines beneath this root.
    let root_session = spec.run_id.as_str().to_string();
    write_run_session(&storage, &root_session)?;
    let config_snapshot = snapshot_proxy_config(&storage, &root_session, &config)?;

    let mut overlay_cfg = config.overlay.clone();
    apply_overlay_override(&mut overlay_cfg, &opts.overlay_override);

    let prepared_overlay = prepare_overlay(&overlay_cfg, &storage, &root_session)?;
    let PreparedOverlay {
        mount: overlay_mount,
        hint: overlay_hint,
        record: overlay_record,
        lowers: overlay_lowers,
    } = prepared_overlay;

    let RunInvocation::Process(process) = &spec.invocation;
    let stage_dir = overlay_record
        .as_ref()
        .map(|record| record.stage_dir.clone())
        .unwrap_or_else(|| storage.clone());
    let lease = RunLease::acquire(&stage_dir)?;
    let run_record = RunRecord {
        schema_version: 1,
        run_id: spec.run_id.as_str().to_string(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: config.agent_id.clone(),
        pid: std::process::id(),
        command: std::iter::once(process.program.clone())
            .chain(process.args.iter().cloned())
            .collect(),
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        overlaynet_listen: Some(gateway.listen.clone()),
        network_interception: Some(persisting_overlaynet::InterceptionProfile::explicit_proxy()),
        network_interception_metrics: None,
        gateway_listen: opts.gateway_enabled.then(|| gateway.listen.clone()),
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy: Some(serde_json::to_value(&config.network)?),
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

    enrich_with_session(
        spec,
        SessionImplantOpts {
            listen: &gateway.listen,
            root_session: &root_session,
            overlay: &overlay_hint,
            overlay_record: overlay_record.as_ref(),
            storage: &storage,
            config_path: &config_snapshot,
            gateway_enabled: opts.gateway_enabled,
        },
    )?;

    Ok(AttemptSession {
        root_session,
        agent_id: config.agent_id.clone(),
        overlay_record,
        gateway: Some(gateway),
        overlay: overlay_mount,
        sink: Some(sink),
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
}

/// Prepare a durable OverlayFS Run without enabling the optional Gateway.
pub fn prepare_overlay_attempt(
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
    let prepared_overlay = prepare_overlay(&overlay_cfg, &storage, &root_session)?;
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
    let lease = RunLease::acquire(&overlay_record.stage_dir)?;
    let run_record = RunRecord {
        schema_version: 1,
        run_id: spec.run_id.as_str().to_string(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: spec.agent.name.clone(),
        pid: std::process::id(),
        command: std::iter::once(process.program.clone())
            .chain(process.args.iter().cloned())
            .collect(),
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        overlaynet_listen: None,
        network_interception: None,
        network_interception_metrics: None,
        gateway_listen: None,
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy: None,
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
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());

    Ok(AttemptSession {
        root_session,
        agent_id: spec.agent.name.clone(),
        overlay_record: Some(overlay_record),
        gateway: None,
        overlay: overlay_mount,
        sink: None,
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
}

/// Prepare a metadata-only durable Run workspace without Gateway or OverlayFS.
pub fn prepare_storage_attempt(
    spec: &mut RunSpec,
    storage: &Path,
) -> anyhow::Result<AttemptSession> {
    let storage = storage
        .canonicalize()
        .unwrap_or_else(|_| storage.to_path_buf());
    let root_session = spec.run_id.as_str().to_string();
    let RunInvocation::Process(process) = &spec.invocation;
    let lease = RunLease::acquire(&storage)?;
    let run_record = RunRecord {
        schema_version: 1,
        run_id: root_session.clone(),
        parent_run_id: spec.parent_run_id.as_ref().map(ToString::to_string),
        task_id: spec.task_id.clone(),
        session_id: root_session.clone(),
        agent: spec.agent.name.clone(),
        pid: std::process::id(),
        command: std::iter::once(process.program.clone())
            .chain(process.args.iter().cloned())
            .collect(),
        executor: executor_from_spec(spec),
        state: "running".into(),
        started_at_unix_ms: crate::util::unix_now_ms(),
        finished_at_unix_ms: None,
        storage: storage.clone(),
        overlaynet_listen: None,
        network_interception: None,
        network_interception_metrics: None,
        gateway_listen: None,
        network: serde_json::to_value(&spec.capabilities.network)?,
        network_policy: None,
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
        notes: vec![format!("durable Run workspace: {}", storage.display())],
    };
    plan.env
        .insert("PERSISTING_RUN_ID".into(), root_session.clone());
    plan.env
        .insert("PERSISTING_AGENT".into(), spec.agent.name.clone());
    plan.env.insert(
        "PERSISTING_PVISOR_STORAGE".into(),
        storage.display().to_string(),
    );
    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    spec.metadata
        .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());

    Ok(AttemptSession {
        root_session,
        agent_id: spec.agent.name.clone(),
        overlay_record: None,
        gateway: None,
        overlay: None,
        sink: None,
        started_at: Instant::now(),
        run_record,
        _control: control,
        _lease: lease,
    })
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

fn executor_from_spec(spec: &RunSpec) -> Option<persisting_proto::ExecutorDescriptor> {
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
        // First lower treated as target when target unset.
        if overlay_cfg.target.is_none() {
            overlay_cfg.target = Some(overlay_override.lower_dirs[0].display().to_string());
            overlay_cfg.lower_dirs = overlay_override.lower_dirs[1..]
                .iter()
                .map(|p| p.display().to_string())
                .collect();
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
            let mount = mount_overlay_record(&record, &lowers)?;
            let record = mount.record().clone();
            let hint = hint_from_record(&record, lowers.clone());
            Ok(PreparedOverlay {
                mount: Some(mount),
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

struct SessionImplantOpts<'a> {
    listen: &'a str,
    root_session: &'a str,
    overlay: &'a OverlayHint,
    overlay_record: Option<&'a OverlayRecord>,
    storage: &'a Path,
    config_path: &'a Path,
    gateway_enabled: bool,
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
        storage,
        config_path,
        gateway_enabled,
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
        storage.display().to_string(),
    );
    plan.notes.push("capture: in-process proxy started".into());

    for (key, value) in proxy_environment(listen, root_session) {
        plan.env.insert(key, value);
    }
    plan.notes
        .push(format!("capture: proxy env → http://{listen}"));
    plan.env.insert(
        "PERSISTING_OVERLAYNET_DRIVER".into(),
        "explicit-proxy".into(),
    );
    plan.env.insert(
        "PERSISTING_OVERLAYNET_STRENGTH".into(),
        "cooperative".into(),
    );
    plan.notes.push(
        "network interception: explicit proxy (cooperative; direct sockets remain ambient)".into(),
    );

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
            plan.notes
                .push("network: deny for traffic intercepted by the proxy".into());
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
                "network: allowlist ({} legacy hosts, {} structured rules, applied to intercepted proxy traffic)",
                hosts.len(),
                rules.len()
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
                    persisting_proto::NetworkDefaultAction::Allow => "default-allow",
                    persisting_proto::NetworkDefaultAction::Deny => "default-deny",
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
                "network: policy ({} allow, {} deny, {} bandwidth limits, applied to intercepted proxy traffic)",
                allow.len(),
                deny.len(),
                limits.len()
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
    if process.cwd.is_none() {
        if let Some(cwd) = &plan.cwd {
            process.cwd = Some(cwd.display().to_string());
        }
    }
}
