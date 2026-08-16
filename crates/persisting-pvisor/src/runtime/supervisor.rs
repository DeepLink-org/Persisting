use super::attempt::{
    apply_implant, prepare_attempt, prepare_overlay_attempt, prepare_storage_attempt,
    AttemptPrepareOpts, AttemptSession, OverlayAttemptPrepareOpts,
};
use super::implant::{ImplantPlan, OverlayHint};
use crate::TrajectoryEventSink;
use crate::{GatewayDriverConfig, NetworkDriverConfig, OverlayNetMode};
use persisting_agentctl::{AttemptId, NetworkCapability, RunSpec};
use persisting_agentctl::{ControlController, PolicyControlController};
use persisting_gateway::config::ProxyConfig;
use std::path::PathBuf;
use std::sync::Arc;

/// Runtime features and strong capability enforcement available for one Attempt.
///
/// `network` and `filesystem` report non-bypassable policy enforcement, not
/// proxy injection or a staged filesystem projection.
#[derive(Debug, Clone)]
pub struct RuntimeCapabilities {
    pub agentctl: bool,
    pub gateway: bool,
    pub network: bool,
    pub filesystem: bool,
    pub providers: Vec<&'static str>,
    /// Network interception support available to a VM Attempt. This is not a
    /// claim that every configured executor is currently enforcing it.
    pub vm_network: bool,
}

impl Default for RuntimeCapabilities {
    fn default() -> Self {
        let vm_network = vm_network_supported();
        let mut providers = vec![
            "local-process",
            "agentctl-unix-v1",
            "in-process-capture",
            "overlaynet-explicit-proxy",
            "fs-overlay-staging",
        ];
        if vm_network {
            providers.push("overlaynet-vm-smoltcp");
        }
        Self {
            agentctl: true,
            gateway: true,
            network: false,
            filesystem: false,
            providers,
            vm_network,
        }
    }
}

fn vm_network_supported() -> bool {
    cfg!(any(
        target_os = "linux",
        all(target_os = "macos", target_arch = "aarch64")
    ))
}

fn network_config_from_capability(
    capability: &NetworkCapability,
) -> persisting_overlaynet::NetworkConfig {
    use persisting_agentctl::NetworkDefaultAction;
    use persisting_overlaynet::NetworkMode;

    match capability {
        NetworkCapability::Ambient => persisting_overlaynet::NetworkConfig::default(),
        NetworkCapability::Deny => persisting_overlaynet::NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..Default::default()
        },
        NetworkCapability::AllowList { hosts, rules } => persisting_overlaynet::NetworkConfig {
            mode: NetworkMode::Allowlist,
            allowed_hosts: hosts.clone(),
            rules: rules.clone(),
            ..Default::default()
        },
        NetworkCapability::Policy {
            default_action,
            allow,
            deny,
            limits,
        } => persisting_overlaynet::NetworkConfig {
            mode: match default_action {
                NetworkDefaultAction::Allow => NetworkMode::Public,
                NetworkDefaultAction::Deny => NetworkMode::Allowlist,
            },
            allowed_hosts: Vec::new(),
            rules: allow.clone(),
            deny_rules: deny.clone(),
            limits: limits.clone(),
        },
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
    network: Option<NetworkDriverConfig>,
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
            .field("network", &self.network)
            .finish_non_exhaustive()
    }
}

impl RuntimeSupervisorBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn gateway(mut self, gateway: GatewayDriverConfig) -> Self {
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

    pub fn network(mut self, network: NetworkDriverConfig) -> Self {
        self.network = Some(network);
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
            network: self.network,
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
    network: Option<NetworkDriverConfig>,
}

impl Default for RuntimeSupervisor {
    fn default() -> Self {
        RuntimeSupervisorBuilder::new().build()
    }
}

impl RuntimeSupervisor {
    fn network_mode(&self) -> OverlayNetMode {
        self.network
            .as_ref()
            .map_or(OverlayNetMode::Auto, |network| network.mode)
    }

    fn effective_network_config(&self, spec: &RunSpec) -> persisting_overlaynet::NetworkConfig {
        self.network
            .as_ref()
            .map(|network| network.network.clone())
            .or_else(|| self.proxy.as_ref().map(|proxy| proxy.network.clone()))
            .unwrap_or_else(|| network_config_from_capability(&spec.capabilities.network))
    }

    pub(crate) fn vm_network_is_enforcing(&self) -> bool {
        self.network_mode() == OverlayNetMode::Auto && vm_network_supported()
    }

    pub(crate) fn vm_network_is_requested(&self) -> bool {
        self.network_mode() == OverlayNetMode::Auto
    }

    pub(crate) fn proxy_network_is_configured(&self) -> bool {
        self.proxy.is_some()
    }

    pub(crate) fn apply_network_capability(&self, spec: &mut RunSpec) {
        let network = self.effective_network_config(spec);
        spec.capabilities.network = persisting_overlaynet::policy::network_capability(&network);
    }

    fn vm_network_options(
        &self,
        mut network: persisting_overlaynet::NetworkConfig,
        supervisor_limits: &[persisting_agentctl::NetworkBandwidthLimit],
        attempt_id: &AttemptId,
    ) -> super::attempt::VmNetworkPrepareOpts {
        network.limits.extend_from_slice(supervisor_limits);
        super::attempt::VmNetworkPrepareOpts {
            network,
            controller: Arc::clone(&self.controller),
            attempt_id: attempt_id.to_string(),
        }
    }

    pub fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities::default()
    }

    /// Start configured pVisor drivers and merge their implant into `spec`.
    pub fn prepare(
        &self,
        spec: &mut RunSpec,
        supervisor_limits: &[persisting_agentctl::NetworkBandwidthLimit],
        vm_executor: bool,
        attempt_id: &AttemptId,
    ) -> anyhow::Result<Option<AttemptSession>> {
        let network_mode = self.network_mode();
        let network = self.effective_network_config(spec);
        let vm_network = vm_executor && network_mode == OverlayNetMode::Auto;
        if vm_executor && network_mode == OverlayNetMode::Proxy {
            anyhow::bail!("overlaynet mode `proxy` is only valid for host/container execution; use `auto` for VM smoltcp networking");
        }
        if vm_executor && network_mode == OverlayNetMode::Off && self.proxy.is_some() {
            anyhow::bail!(
                "overlaynet mode `off` makes the VM offline and cannot be combined with Gateway/proxy configuration"
            );
        }
        if let Some(proxy) = &self.proxy {
            let mut proxy = proxy.clone();
            // NetworkDriverConfig is the one Attempt policy source. ProxyConfig
            // retains its field for standalone Gateway use only.
            if vm_network {
                proxy.network = self
                    .vm_network_options(network, supervisor_limits, attempt_id)
                    .network;
            } else {
                proxy.network = network;
                proxy.network.limits.extend_from_slice(supervisor_limits);
            }
            let storage = self
                .storage
                .clone()
                .unwrap_or_else(|| PathBuf::from(".persisting/capture"));
            let capture_storage = self
                .gateway_output_dir
                .clone()
                .unwrap_or_else(|| storage.clone());
            let session = prepare_attempt(
                spec,
                AttemptPrepareOpts {
                    config: &proxy,
                    storage: &storage,
                    capture_storage: &capture_storage,
                    sink: self.sink.clone(),
                    stream_markdown: self.stream_markdown,
                    overlay_override: self.overlay.clone(),
                    controller: Arc::clone(&self.controller),
                    gateway_enabled: self.gateway_enabled,
                    vm_network,
                    attempt_id: attempt_id.as_str(),
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
            let session = prepare_overlay_attempt(
                spec,
                OverlayAttemptPrepareOpts {
                    storage: &storage,
                    overlay: self.overlay.clone(),
                    vm_network: vm_network
                        .then(|| self.vm_network_options(network, supervisor_limits, attempt_id)),
                },
            )?;
            return Ok(Some(session));
        }

        if let Some(storage) = &self.storage {
            let session = prepare_storage_attempt(
                spec,
                storage,
                vm_network.then(|| self.vm_network_options(network, supervisor_limits, attempt_id)),
            )?;
            return Ok(Some(session));
        }

        if vm_network {
            let storage = super::registry::default_run_home().join(spec.run_id.as_str());
            std::fs::create_dir_all(&storage)?;
            let session = prepare_storage_attempt(
                spec,
                &storage,
                Some(self.vm_network_options(network, supervisor_limits, attempt_id)),
            )?;
            return Ok(Some(session));
        }

        let _ = self.enrich_spec(spec);
        Ok(None)
    }

    /// Build the implant plan and merge it into a process RunSpec (env markers only).
    pub fn enrich_spec(&self, spec: &mut RunSpec) -> ImplantPlan {
        let plan = self.plan_for(spec);
        let persisting_agentctl::RunInvocation::Process(ref mut process) = spec.invocation;
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
            plan.env.insert(
                "PERSISTING_OVERLAYNET_DRIVER".into(),
                "explicit-proxy".into(),
            );
            plan.env.insert(
                "PERSISTING_OVERLAYNET_STRENGTH".into(),
                "cooperative".into(),
            );
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
                plan.notes.push(
                    "network: deny requested; only intercepted proxy traffic is controlled".into(),
                );
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
                    "network: allowlist ({} legacy hosts, {} structured rules)",
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
                    "network: policy ({} allow, {} deny, {} bandwidth limits)",
                    allow.len(),
                    deny.len(),
                    limits.len()
                ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_gateway::config::ProxyConfig;

    fn test_proxy() -> ProxyConfig {
        ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:19081"
admin_listen = "127.0.0.1:19876"
agent_id = "test"
models = []
"#,
        )
        .unwrap()
    }

    #[test]
    fn capabilities_report_vm_smoltcp_only_on_supported_hosts() {
        let capabilities = RuntimeCapabilities::default();
        assert_eq!(capabilities.vm_network, vm_network_supported());
        assert_eq!(
            capabilities.providers.contains(&"overlaynet-vm-smoltcp"),
            vm_network_supported()
        );
    }

    #[test]
    fn explicit_network_config_is_the_attempt_policy_source() {
        let mut proxy = test_proxy();
        proxy.network.mode = persisting_overlaynet::NetworkMode::Public;
        let supervisor = RuntimeSupervisorBuilder::new()
            .gateway(GatewayDriverConfig::new(proxy))
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Proxy,
                persisting_overlaynet::NetworkConfig {
                    mode: persisting_overlaynet::NetworkMode::NoNetwork,
                    ..Default::default()
                },
            ))
            .build();
        let mut spec = RunSpec::process("configured-policy", "test", "true");

        supervisor.apply_network_capability(&mut spec);

        assert_eq!(spec.capabilities.network, NetworkCapability::Deny);
    }

    #[test]
    fn absent_network_config_preserves_the_run_spec_policy() {
        let supervisor = RuntimeSupervisorBuilder::new().build();
        let mut spec = RunSpec::process("spec-policy", "test", "true");
        spec.capabilities.network = NetworkCapability::Deny;

        supervisor.apply_network_capability(&mut spec);

        assert_eq!(spec.capabilities.network, NetworkCapability::Deny);
    }

    #[test]
    fn network_capability_roundtrips_into_driver_config() {
        let cases = [
            NetworkCapability::Ambient,
            NetworkCapability::Deny,
            NetworkCapability::AllowList {
                hosts: vec!["api.example.com".into()],
                rules: Vec::new(),
            },
            NetworkCapability::Policy {
                default_action: persisting_agentctl::NetworkDefaultAction::Deny,
                allow: vec![persisting_agentctl::NetworkAccessRule {
                    host: "api.example.com".into(),
                    ports: vec![443],
                    transports: vec![persisting_agentctl::NetworkTransport::TcpTunnel],
                    allow_private_ips: false,
                }],
                deny: vec![persisting_agentctl::NetworkAccessRule {
                    host: "metadata.internal".into(),
                    ports: Vec::new(),
                    transports: Vec::new(),
                    allow_private_ips: false,
                }],
                limits: Vec::new(),
            },
        ];

        for capability in cases {
            let config = network_config_from_capability(&capability);
            assert_eq!(
                persisting_overlaynet::policy::network_capability(&config),
                capability
            );
        }
    }

    #[test]
    fn offline_vm_rejects_gateway_configuration() {
        let supervisor = RuntimeSupervisorBuilder::new()
            .network(NetworkDriverConfig::new(
                OverlayNetMode::Off,
                Default::default(),
            ))
            .gateway(GatewayDriverConfig::new(test_proxy()))
            .build();
        let mut spec = RunSpec::process("offline-vm", "test", "true");
        let error =
            match supervisor.prepare(&mut spec, &[], true, &AttemptId::new("attempt-offline")) {
                Ok(_) => panic!("offline VM accepted Gateway configuration"),
                Err(error) => error,
            };
        assert!(error
            .to_string()
            .contains("mode `off` makes the VM offline"));
    }
}
