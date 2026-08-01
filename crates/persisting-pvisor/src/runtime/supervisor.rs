use super::attempt::{
    apply_implant, prepare_attempt, prepare_overlay_attempt, AttemptPrepareOpts, AttemptSession,
    OverlayAttemptPrepareOpts,
};
use super::implant::{ImplantPlan, OverlayHint};
use crate::GatewayDriverConfig;
use crate::TrajectoryEventSink;
use persisting_control::{ControlController, PolicyControlController};
use persisting_gateway::config::ProxyConfig;
use persisting_proto::{NetworkCapability, RunSpec};
use std::path::PathBuf;
use std::sync::Arc;

/// Runtime features and strong capability enforcement available for one Attempt.
///
/// `network` and `filesystem` report non-bypassable policy enforcement, not
/// proxy injection or a staged filesystem projection.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    pub gateway: bool,
    pub network: bool,
    pub filesystem: bool,
    pub providers: Vec<&'static str>,
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self {
            gateway: true,
            network: false,
            filesystem: false,
            providers: vec![
                "local-process",
                "in-process-capture",
                "overlaynet-explicit-proxy",
                "fs-overlay-staging",
            ],
        }
    }
}

/// Builder for Attempt prepare options (capture / overlay). Crate-private;
/// public configuration goes through [`crate::PVisorBuilder`].
#[derive(Clone, Default)]
pub struct RuntimeSupervisorBuilder {
    proxy: Option<ProxyConfig>,
    gateway_output_dir: Option<PathBuf>,
    gateway_enabled: bool,
    storage: Option<PathBuf>,
    stream_markdown: bool,
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    overlay: OverlayHint,
    controller: Option<Arc<dyn ControlController>>,
}

impl std::fmt::Debug for RuntimeSupervisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSupervisorBuilder")
            .field("proxy", &self.proxy.as_ref().map(|_| "<ProxyConfig>"))
            .field("gateway_output_dir", &self.gateway_output_dir)
            .field("storage", &self.storage)
            .field("stream_markdown", &self.stream_markdown)
            .field("sink", &self.sink.as_ref().map(|_| "<TrajectoryEventSink>"))
            .field("overlay", &self.overlay)
            .finish_non_exhaustive()
    }
}

impl RuntimeSupervisorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.storage = Some(gateway.output_dir.clone());
        self.proxy = Some(gateway.proxy);
        self.gateway_output_dir = Some(gateway.output_dir);
        self.stream_markdown = gateway.stream_markdown;
        self.gateway_enabled = gateway.gateway_enabled;
        self
    }

    pub fn storage(mut self, storage: PathBuf) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn trajectory_sink(mut self, sink: Arc<dyn TrajectoryEventSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn overlay(mut self, overlay: OverlayHint) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn control_controller(mut self, controller: Arc<dyn ControlController>) -> Self {
        self.controller = Some(controller);
        self
    }

    pub fn build(self) -> RuntimeSupervisor {
        RuntimeSupervisor {
            proxy: self.proxy,
            gateway_output_dir: self.gateway_output_dir,
            gateway_enabled: self.gateway_enabled,
            storage: self.storage,
            stream_markdown: self.stream_markdown,
            sink: self.sink,
            overlay: self.overlay,
            controller: self
                .controller
                .unwrap_or_else(|| Arc::new(PolicyControlController)),
        }
    }
}

/// Capture / network / overlay prepare options for one Attempt.
#[derive(Clone)]
pub struct RuntimeSupervisor {
    proxy: Option<ProxyConfig>,
    gateway_output_dir: Option<PathBuf>,
    gateway_enabled: bool,
    storage: Option<PathBuf>,
    stream_markdown: bool,
    sink: Option<Arc<dyn TrajectoryEventSink>>,
    overlay: OverlayHint,
    controller: Arc<dyn ControlController>,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        RuntimeSupervisorBuilder::new().build()
    }
}

impl RuntimeSupervisor {
    pub fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }

    /// Start configured pVisor drivers and merge their implant into `spec`.
    pub fn prepare(&self, spec: &mut RunSpec) -> anyhow::Result<Option<AttemptSession>> {
        if let Some(proxy) = &self.proxy {
            let storage = self
                .storage
                .clone()
                .unwrap_or_else(|| PathBuf::from(".persisting/capture"));
            let (session, _plan) = prepare_attempt(
                spec,
                AttemptPrepareOpts {
                    config: proxy,
                    storage: &storage,
                    sink: self.sink.clone(),
                    stream_markdown: self.stream_markdown,
                    overlay_override: self.overlay.clone(),
                    controller: Arc::clone(&self.controller),
                    gateway_enabled: self.gateway_enabled,
                },
            )?;
            return Ok(Some(session));
        }

        if !self.overlay.lower_dirs.is_empty()
            || self.overlay.stage_dir.is_some()
            || self.overlay.upper_dir.is_some()
            || self.overlay.work_dir.is_some()
            || self.overlay.merged_dir.is_some()
        {
            let storage = self
                .storage
                .clone()
                .unwrap_or_else(|| PathBuf::from(".persisting/capture"));
            let (session, _plan) = prepare_overlay_attempt(
                spec,
                OverlayAttemptPrepareOpts {
                    storage: &storage,
                    overlay: self.overlay.clone(),
                },
            )?;
            return Ok(Some(session));
        }

        let _ = self.enrich_spec(spec);
        Ok(None)
    }

    /// Build the implant plan and merge it into a process RunSpec (env markers only).
    pub fn enrich_spec(&self, spec: &mut RunSpec) -> ImplantPlan {
        let plan = self.plan_for(spec);
        let persisting_proto::RunInvocation::Process(ref mut process) = spec.invocation;
        apply_implant(process, &plan);
        spec.metadata
            .insert("pvisor.runtime.implant".into(), plan.as_metadata_json());
        plan
    }

    pub fn plan_for(&self, spec: &RunSpec) -> ImplantPlan {
        let mut plan = ImplantPlan {
            env: ImplantPlan::marker_env(),
            cwd: self.overlay.merged_dir.clone(),
            overlay: self.overlay.clone(),
            notes: Vec::new(),
        };

        plan.env
            .insert("PERSISTING_RUN_ID".into(), spec.run_id.as_str().to_string());
        plan.env
            .insert("PERSISTING_AGENT".into(), spec.agent.name.clone());

        if self.proxy.is_some() {
            plan.notes
                .push("network: in-process OverlayNet proxy configured".into());
        }
        if let Some(path) = &self.gateway_output_dir {
            plan.env.insert(
                "PERSISTING_CAPTURE_STORAGE".into(),
                path.display().to_string(),
            );
            plan.notes.push("capture: storage path exported".into());
        }

        match &spec.capabilities.network {
            NetworkCapability::Ambient => {
                plan.env
                    .insert("PERSISTING_NETWORK_POLICY".into(), "ambient".into());
                plan.notes.push("network: ambient".into());
            }
            NetworkCapability::Deny => {
                plan.env
                    .insert("PERSISTING_NETWORK_POLICY".into(), "deny".into());
                plan.notes
                    .push("network: deny (interposed by control-aware capture proxy)".into());
            }
            NetworkCapability::AllowList { hosts } => {
                plan.env
                    .insert("PERSISTING_NETWORK_POLICY".into(), "allowlist".into());
                plan.env
                    .insert("PERSISTING_NETWORK_ALLOWLIST".into(), hosts.join(","));
                plan.notes
                    .push(format!("network: allowlist ({} hosts)", hosts.len()));
            }
        }

        if self.overlay.merged_dir.is_some() {
            plan.notes
                .push("filesystem: merged overlay root selected as cwd".into());
        } else {
            plan.notes
                .push("filesystem: host view (no overlay merged_dir)".into());
        }

        plan
    }
}
