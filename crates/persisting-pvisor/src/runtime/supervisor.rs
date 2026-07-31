use super::attempt::{apply_implant, prepare_attempt, AttemptPrepareOpts, AttemptSession};
use super::implant::{ImplantPlan, OverlayHint};
use persisting_access::PolicyAccessController;
use persisting_capture::sink::CaptureSink;
use persisting_proto::{NetworkCapability, RunSpec};
use std::path::PathBuf;
use std::sync::Arc;

/// What the in-guest runtime can enforce for one Attempt.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    pub capture: bool,
    pub network: bool,
    pub filesystem: bool,
    pub providers: Vec<&'static str>,
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        Self {
            capture: true,
            network: true,
            filesystem: true,
            providers: vec![
                "local-process",
                "in-process-capture",
                "network-policy",
                "fs-overlay",
            ],
        }
    }
}

/// Builder for Attempt prepare options (capture / overlay). Crate-private;
/// public configuration goes through [`crate::PVisorBuilder`].
#[derive(Clone, Default)]
pub struct RuntimeSupervisorBuilder {
    capture_https_proxy: Option<String>,
    capture_http_proxy: Option<String>,
    capture_config_path: Option<PathBuf>,
    capture_output_dir: Option<PathBuf>,
    stream_markdown: bool,
    sink: Option<Arc<dyn CaptureSink>>,
    overlay: OverlayHint,
    access: Option<Arc<PolicyAccessController>>,
}

impl std::fmt::Debug for RuntimeSupervisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeSupervisorBuilder")
            .field("capture_https_proxy", &self.capture_https_proxy)
            .field("capture_http_proxy", &self.capture_http_proxy)
            .field("capture_config_path", &self.capture_config_path)
            .field("capture_output_dir", &self.capture_output_dir)
            .field("stream_markdown", &self.stream_markdown)
            .field("sink", &self.sink.as_ref().map(|_| "<CaptureSink>"))
            .field("overlay", &self.overlay)
            .finish_non_exhaustive()
    }
}

impl RuntimeSupervisorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn capture_https_proxy(mut self, url: impl Into<String>) -> Self {
        self.capture_https_proxy = Some(url.into());
        self
    }

    pub fn capture_http_proxy(mut self, url: impl Into<String>) -> Self {
        self.capture_http_proxy = Some(url.into());
        self
    }

    pub fn capture_config(mut self, path: impl Into<PathBuf>) -> Self {
        self.capture_config_path = Some(path.into());
        self
    }

    pub fn capture_output_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.capture_output_dir = Some(path.into());
        self
    }

    pub fn stream_markdown(mut self, enabled: bool) -> Self {
        self.stream_markdown = enabled;
        self
    }

    pub fn capture_sink(mut self, sink: Arc<dyn CaptureSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    pub fn overlay(mut self, overlay: OverlayHint) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn access_controller(mut self, access: Arc<PolicyAccessController>) -> Self {
        self.access = Some(access);
        self
    }

    pub fn build(self) -> RuntimeSupervisor {
        RuntimeSupervisor {
            capture_https_proxy: self.capture_https_proxy,
            capture_http_proxy: self.capture_http_proxy,
            capture_config_path: self.capture_config_path,
            capture_output_dir: self.capture_output_dir,
            stream_markdown: self.stream_markdown,
            sink: self.sink,
            overlay: self.overlay,
            access: self
                .access
                .unwrap_or_else(|| Arc::new(PolicyAccessController)),
        }
    }
}

/// Capture / network / overlay prepare options for one Attempt.
#[derive(Clone)]
pub struct RuntimeSupervisor {
    capture_https_proxy: Option<String>,
    capture_http_proxy: Option<String>,
    capture_config_path: Option<PathBuf>,
    capture_output_dir: Option<PathBuf>,
    stream_markdown: bool,
    sink: Option<Arc<dyn CaptureSink>>,
    overlay: OverlayHint,
    #[allow(dead_code)] // reserved for serve_with_runtime_control wiring
    access: Arc<PolicyAccessController>,
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

    /// True when this prepare path will start an in-process capture proxy from config.
    pub fn enforces_via_capture(&self) -> bool {
        self.capture_config_path.is_some()
    }

    /// Start capture + overlay (when configured) and merge implant into `spec`.
    pub fn prepare(&self, spec: &mut RunSpec) -> anyhow::Result<Option<AttemptSession>> {
        if let Some(config_path) = &self.capture_config_path {
            let storage = self
                .capture_output_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(".persisting/capture"));
            let (session, _plan) = prepare_attempt(
                spec,
                AttemptPrepareOpts {
                    config_path,
                    storage: &storage,
                    sink: self.sink.clone(),
                    stream_markdown: self.stream_markdown,
                    overlay_override: self.overlay.clone(),
                },
            )?;
            return Ok(Some(session));
        }

        // Legacy path: env-only implant (external/daemon capture).
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

        if let Some(url) = &self.capture_https_proxy {
            plan.env.insert("HTTPS_PROXY".into(), url.clone());
            plan.env.insert("https_proxy".into(), url.clone());
            plan.notes.push("capture: HTTPS_PROXY injected".into());
        }
        if let Some(url) = &self.capture_http_proxy {
            plan.env.insert("HTTP_PROXY".into(), url.clone());
            plan.env.insert("http_proxy".into(), url.clone());
            plan.notes.push("capture: HTTP_PROXY injected".into());
        }
        if let Some(path) = &self.capture_config_path {
            plan.env.insert(
                "PERSISTING_CAPTURE_CONFIG".into(),
                path.display().to_string(),
            );
            plan.notes
                .push("capture: config path set (call prepare() to start in-process proxy)".into());
        }
        if let Some(path) = &self.capture_output_dir {
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
                    .push("network: deny (enforced by access controller / capture proxy)".into());
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
