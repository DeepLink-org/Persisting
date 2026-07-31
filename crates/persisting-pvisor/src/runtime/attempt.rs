//! Attempt-scoped capture + overlay session owned by pVisor prepare.

use super::implant::{ImplantPlan, OverlayHint};
use super::overlay::{
    apply_overlay, hint_from_record, lower_stack_from_config, mount_overlay_record,
    resolve_overlay_workspace, OverlayMount, OverlayRecord,
};
use persisting_capture::config::ProxyConfig;
use persisting_capture::injection::{client_gateway_config_args, proxy_environment};
use persisting_capture::lifecycle::{
    append_lifecycle, root_session_route, session_ended_record, session_started_record, CaptureMode,
};
use persisting_capture::proxy::network_capability_from_config;
use persisting_capture::runtime::in_process::InProcessCapture;
use persisting_capture::runtime::run_config::snapshot_run_proxy_config;
use persisting_capture::runtime::run_env::{
    apply_daemon_env, snapshot_daemon_env, strip_capture_proxy_env, write_run_session,
};
use persisting_capture::sink::{CaptureSink, SeqOnlySink};
use persisting_proto::{NetworkCapability, ProcessInvocation, RunInvocation, RunSpec};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Live controls for one Attempt: capture proxy + optional overlay mount.
pub struct AttemptSession {
    pub root_session: String,
    pub listen: String,
    pub agent_id: String,
    pub config_path: PathBuf,
    pub storage: PathBuf,
    /// Staging record retained after unmount (for apply / discard).
    pub overlay_record: Option<OverlayRecord>,
    capture: Option<InProcessCapture>,
    overlay: Option<OverlayMount>,
    sink: Arc<dyn CaptureSink>,
    started_at: Instant,
}

impl AttemptSession {
    pub fn teardown(mut self, exit_code: Option<i32>) -> anyhow::Result<()> {
        let duration_ms = self.started_at.elapsed().as_millis() as u64;
        if let Err(err) = append_lifecycle(
            self.sink.as_ref(),
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
            tracing::warn!(error = %err, "failed to append session.ended");
        }

        let mut record = if let Some(mount) = self.overlay.take() {
            Some(mount.unmount()?)
        } else {
            self.overlay_record.take()
        };

        if let Some(ref mut rec) = record {
            if rec.auto_apply {
                if let Err(err) = apply_overlay(rec) {
                    tracing::warn!(error = %err, "overlay auto_apply failed");
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
                     `persisting runtime overlay apply -o <storage> --id {}` \
                     or `… discard`",
                    rec.id
                );
            }
        }
        self.overlay_record = record;

        if let Some(capture) = self.capture.take() {
            capture.shutdown()?;
        }
        Ok(())
    }
}

pub struct AttemptPrepareOpts<'a> {
    pub config_path: &'a Path,
    pub storage: &'a Path,
    pub sink: Option<Arc<dyn CaptureSink>>,
    pub stream_markdown: bool,
    /// Extra overlay hint from CLI (overrides paths when set).
    pub overlay_override: OverlayHint,
}

/// Start capture + overlay from the shared capture TOML, then enrich `spec`.
pub fn prepare_attempt(
    spec: &mut RunSpec,
    opts: AttemptPrepareOpts<'_>,
) -> anyhow::Result<(AttemptSession, ImplantPlan)> {
    let config = ProxyConfig::from_file(opts.config_path).map_err(|err| {
        anyhow::anyhow!(
            "load capture config {}: {err:#}",
            opts.config_path.display()
        )
    })?;
    let storage = opts
        .storage
        .canonicalize()
        .unwrap_or_else(|_| opts.storage.to_path_buf());

    spec.capabilities.network = network_capability_from_config(&config);

    strip_capture_proxy_env();
    snapshot_daemon_env(&storage, &config)?;
    let _ = apply_daemon_env(&storage)?;

    let sink = opts
        .sink
        .unwrap_or_else(|| Arc::new(SeqOnlySink::new()) as Arc<dyn CaptureSink>);

    let capture = InProcessCapture::start(
        config.clone(),
        storage.clone(),
        Arc::clone(&sink),
        opts.stream_markdown,
    )?;

    let root_session = format!("run-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S-%f"));
    write_run_session(&storage, &root_session)?;
    snapshot_run_proxy_config(&storage, &root_session, opts.config_path)?;

    let mut overlay_cfg = config.overlay.clone();
    // CLI overrides
    if let Some(merged) = &opts.overlay_override.merged_dir {
        overlay_cfg.merged_dir = Some(merged.display().to_string());
        overlay_cfg.enabled = true;
    }
    if let Some(upper) = &opts.overlay_override.upper_dir {
        overlay_cfg.upper_dir = Some(upper.display().to_string());
        overlay_cfg.backend = persisting_capture::config::OverlayBackend::Directory;
        overlay_cfg.database_path = None;
    }
    if let Some(work) = &opts.overlay_override.work_dir {
        overlay_cfg.work_dir = Some(work.display().to_string());
        overlay_cfg.backend = persisting_capture::config::OverlayBackend::Directory;
        overlay_cfg.database_path = None;
    }
    if !opts.overlay_override.lower_dirs.is_empty() {
        // First lower treated as target when target unset.
        if overlay_cfg.target.is_none() {
            overlay_cfg.target = Some(opts.overlay_override.lower_dirs[0].display().to_string());
            overlay_cfg.lower_dirs = opts.overlay_override.lower_dirs[1..]
                .iter()
                .map(|p| p.display().to_string())
                .collect();
        } else {
            overlay_cfg.lower_dirs = opts
                .overlay_override
                .lower_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect();
        }
        overlay_cfg.enabled = true;
    }

    let (overlay_mount, overlay_hint, overlay_record) =
        if overlay_cfg.enabled || overlay_cfg.target.is_some() {
            match resolve_overlay_workspace(&overlay_cfg, &storage, &root_session)? {
                Some(record) => {
                    let lowers = lower_stack_from_config(&overlay_cfg, &storage, &record.target);
                    let mount = mount_overlay_record(&record, &lowers, None)?;
                    let record = mount.record().clone();
                    let hint = hint_from_record(&record, lowers);
                    (Some(mount), hint, Some(record))
                }
                None => (None, OverlayHint::default(), None),
            }
        } else {
            (None, OverlayHint::default(), None)
        };

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
            Some(&capture.listen),
            Some(program.as_str()),
        ),
    )?;

    let plan = enrich_with_session(
        spec,
        &capture.listen,
        &root_session,
        &overlay_hint,
        overlay_record.as_ref(),
        &storage,
        opts.config_path,
    )?;

    Ok((
        AttemptSession {
            root_session,
            listen: capture.listen.clone(),
            agent_id: config.agent_id.clone(),
            config_path: opts.config_path.to_path_buf(),
            storage,
            overlay_record,
            capture: Some(capture),
            overlay: overlay_mount,
            sink,
            started_at: Instant::now(),
        },
        plan,
    ))
}

fn enrich_with_session(
    spec: &mut RunSpec,
    listen: &str,
    root_session: &str,
    overlay: &OverlayHint,
    overlay_record: Option<&OverlayRecord>,
    storage: &Path,
    config_path: &Path,
) -> anyhow::Result<ImplantPlan> {
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
                .push("network: deny (enforced by capture proxy)".into());
        }
        NetworkCapability::AllowList { hosts } => {
            plan.env
                .insert("PERSISTING_NETWORK_POLICY".into(), "allowlist".into());
            plan.env
                .insert("PERSISTING_NETWORK_ALLOWLIST".into(), hosts.join(","));
            plan.notes.push(format!(
                "network: allowlist ({} hosts, enforced by capture proxy)",
                hosts.len()
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
            super::overlay::OverlayUpper::Redb { database_path } => {
                plan.env.insert(
                    "PERSISTING_OVERLAY_DATABASE".into(),
                    database_path.display().to_string(),
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
            .push("filesystem: fuse-overlayfs merged root as cwd".into());
    } else {
        plan.notes.push("filesystem: host view (no overlay)".into());
    }

    let RunInvocation::Process(ref mut process) = spec.invocation;
    apply_implant(process, &plan);
    inject_gateway_args(process, listen);
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
