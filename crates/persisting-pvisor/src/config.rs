//! Canonical pVisor Run configuration.
//!
//! TOML and the `pvisor run` command line both resolve into [`RunConfig`].
//! Runtime drivers only consume the resolved value and do not read config
//! files themselves.

use std::path::{Path, PathBuf};

use persisting_control::ResourceLimits;
use persisting_gateway::config::{CaptureLevel, ModelRoute, ProxyConfig};
use persisting_overlaynet::{NetworkAccessRule, NetworkBandwidthLimit};
use serde::{Deserialize, Serialize};

use crate::runtime::OverlayHint;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunConfig {
    pub run: RunSettings,
    pub container: ContainerSettings,
    #[serde(alias = "kvm")]
    pub vm: VmSettings,
    /// Transactional filesystem configuration. Absence means host filesystem access.
    pub overlayfs: Option<OverlayFsSettings>,
    pub overlaynet: OverlayNetSettings,
    pub gateway: GatewaySettings,
    pub chronicle: ChronicleSettings,
}

impl RunConfig {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&source)?)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunSettings {
    /// Internally resolved project association; not a user-facing configuration parameter.
    #[serde(skip)]
    pub workspace: Option<PathBuf>,
    pub agent: String,
    pub executor: RunExecutorKind,
    pub timeout_ms: Option<u64>,
    pub stdio: RunStdio,
    pub policy: RunPolicy,
    /// Inherit the complete supervisor environment. Safe CLI runs override
    /// this to false and project only baseline plus explicitly passed keys.
    pub inherit_env: bool,
    /// Host environment variables projected by name when `inherit_env=false`.
    pub pass_env: Vec<String>,
    pub resource_limits: ResourceLimits,
    pub command: Vec<String>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            workspace: None,
            agent: "agent".into(),
            executor: RunExecutorKind::Host,
            timeout_ms: None,
            stdio: RunStdio::Inherit,
            policy: RunPolicy::Observe,
            inherit_env: true,
            pass_env: Vec::new(),
            resource_limits: ResourceLimits::default(),
            command: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunExecutorKind {
    #[default]
    Host,
    Container,
    #[serde(alias = "kvm")]
    Vm,
}

/// OCI CLI configuration used by [`crate::ContainerExecutor`].
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerSettings {
    /// Docker- or Podman-compatible executable.
    pub runtime: PathBuf,
    /// Image reference used for the Agent process.
    pub image: String,
    /// Target-specific statically linked pVisor injected into the container.
    pub pvisor_binary: Option<PathBuf>,
    /// Explicit OCI platform. Required for packaged artifact auto-discovery
    /// when the image platform cannot be inspected locally.
    pub platform: Option<ContainerPlatform>,
    /// Container network namespace mode.
    pub network: ContainerNetwork,
    /// Container-native working directory used when the Run has no mounted cwd.
    pub workdir: Option<PathBuf>,
    /// Optional container user (`uid`, `uid:gid`, or a named user).
    pub user: Option<String>,
    /// Mount the image root filesystem read-only.
    pub read_only_rootfs: bool,
    /// Additional explicit bind mounts. The runtime automatically mounts the
    /// injected pVisor, delegated control directory, final Run cwd, and capture
    /// configuration when present.
    pub mounts: Vec<ContainerMount>,
}

impl Default for ContainerSettings {
    fn default() -> Self {
        Self {
            runtime: PathBuf::from("docker"),
            image: String::new(),
            pvisor_binary: None,
            platform: None,
            network: ContainerNetwork::Host,
            workdir: None,
            user: None,
            read_only_rootfs: false,
            mounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerPlatform {
    LinuxAmd64,
    LinuxArm64,
}

impl ContainerPlatform {
    pub(crate) const fn oci_value(self) -> &'static str {
        match self {
            Self::LinuxAmd64 => "linux/amd64",
            Self::LinuxArm64 => "linux/arm64",
        }
    }
}

impl std::str::FromStr for ContainerPlatform {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "linux/amd64" | "linux-amd64" | "amd64" | "x86_64" => Ok(Self::LinuxAmd64),
            "linux/arm64" | "linux-arm64" | "arm64" | "aarch64" => Ok(Self::LinuxArm64),
            _ => Err(format!(
                "unsupported container platform `{value}`; expected linux/amd64 or linux/arm64"
            )),
        }
    }
}

/// libkrun process isolation over a pVisor-provided Linux rootfs OverlayFS.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct VmSettings {
    /// Linux root filesystem exported to the libkrun guest.
    pub rootfs: Option<PathBuf>,
    /// OCI image used when no explicit rootfs is supplied.
    pub image: Option<String>,
    /// Content-addressed OCI cache. The platform cache directory is used when omitted.
    pub image_store: Option<PathBuf>,
    /// Reject apply operations that would mutate the configured rootfs lower.
    pub rootfs_immutable: bool,
    /// Optional directory containing libkrunfw. Packaged builds discover it
    /// next to pVisor; source builds use a verified per-user download cache.
    pub library_dir: Option<PathBuf>,
    pub memory_mib: u32,
    pub cpus: u16,
}

impl Default for VmSettings {
    fn default() -> Self {
        Self {
            rootfs: None,
            image: Some(crate::oci::DEFAULT_IMAGE.into()),
            image_store: None,
            rootfs_immutable: false,
            library_dir: None,
            memory_mib: 2048,
            cpus: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerNetwork {
    /// Share the runtime host network. This keeps an in-process Gateway and
    /// OverlayNet proxy reachable at their injected loopback addresses.
    #[default]
    Host,
    Bridge,
    None,
}

impl ContainerNetwork {
    pub(crate) const fn as_runtime_value(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Bridge => "bridge",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContainerMount {
    pub source: PathBuf,
    pub target: PathBuf,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunStdio {
    #[default]
    Inherit,
    Capture,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RunPolicy {
    #[default]
    Observe,
    Enforce,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayFsSettings {
    /// Bottom host layer and default apply destination.
    pub base: Option<PathBuf>,
    /// Absolute path where the staged overlay is exposed inside a libkrun guest.
    pub target: Option<PathBuf>,
    /// Additional read-only layers composed above `base`, in command-line order.
    pub compose: Vec<PathBuf>,
    /// Durable writable stage root. Defaults to the generated per-Run storage directory.
    pub stage: Option<PathBuf>,
    pub backend: OverlayFsBackend,
    pub commit: OverlayFsCommit,
}

impl Default for OverlayFsSettings {
    fn default() -> Self {
        Self {
            base: None,
            target: None,
            compose: Vec::new(),
            stage: None,
            backend: OverlayFsBackend::Directory,
            commit: OverlayFsCommit::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayFsBackend {
    #[default]
    Directory,
    Jujutsu,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayFsCommit {
    #[default]
    Manual,
    Apply,
    Drop,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OverlayNetSettings {
    pub mode: OverlayNetMode,
    pub listen: String,
    pub policy: OverlayNetPolicy,
    pub allow: Vec<String>,
    /// Structured grants for port-, transport-, and address-scoped policy.
    pub rules: Vec<NetworkAccessRule>,
    pub deny: Vec<NetworkAccessRule>,
    pub limits: Vec<NetworkBandwidthLimit>,
}

impl Default for OverlayNetSettings {
    fn default() -> Self {
        Self {
            mode: OverlayNetMode::Auto,
            listen: "127.0.0.1:19081".into(),
            policy: OverlayNetPolicy::Public,
            allow: Vec::new(),
            rules: Vec::new(),
            deny: Vec::new(),
            limits: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayNetMode {
    #[default]
    Auto,
    Off,
    Proxy,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayNetPolicy {
    #[default]
    Public,
    Deny,
    Allowlist,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewaySettings {
    pub mode: GatewayMode,
    pub admin_listen: String,
    pub level: CaptureLevel,
    pub session_header: String,
    pub debug: bool,
    pub stream_markdown: bool,
    pub routes: Vec<ModelRoute>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            mode: GatewayMode::Off,
            admin_listen: "127.0.0.1:9876".into(),
            level: CaptureLevel::Dialogue,
            session_header: "x-persisting-session-id".into(),
            debug: false,
            stream_markdown: false,
            routes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum GatewayMode {
    #[default]
    Off,
    Capture,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChronicleSettings {
    pub mode: ChronicleMode,
    pub dir: Option<PathBuf>,
}

impl Default for ChronicleSettings {
    fn default() -> Self {
        Self {
            mode: ChronicleMode::Off,
            dir: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ChronicleMode {
    #[default]
    Off,
    Lance,
}

/// Resolved configuration for the internal OverlayNet + optional Gateway sink.
#[derive(Debug, Clone)]
pub struct GatewayDriverConfig {
    pub proxy: ProxyConfig,
    pub output_dir: PathBuf,
    pub stream_markdown: bool,
    pub gateway_enabled: bool,
}

/// Programmatic network-driver configuration. `Auto` selects smoltcp for a
/// libkrun VM and otherwise remains inactive unless Gateway/proxy is requested.
#[derive(Debug, Clone)]
pub struct NetworkDriverConfig {
    pub mode: OverlayNetMode,
    pub network: persisting_overlaynet::NetworkConfig,
}

impl Default for NetworkDriverConfig {
    fn default() -> Self {
        Self {
            mode: OverlayNetMode::Auto,
            network: persisting_overlaynet::NetworkConfig::default(),
        }
    }
}

impl NetworkDriverConfig {
    pub fn new(mode: OverlayNetMode, network: persisting_overlaynet::NetworkConfig) -> Self {
        Self { mode, network }
    }
}

impl GatewayDriverConfig {
    pub fn new(proxy: ProxyConfig) -> Self {
        Self {
            proxy,
            output_dir: PathBuf::from(".persisting/run"),
            stream_markdown: false,
            gateway_enabled: true,
        }
    }

    pub fn output_dir(mut self, output_dir: impl Into<PathBuf>) -> Self {
        self.output_dir = output_dir.into();
        self
    }

    pub fn stream_markdown(mut self, enabled: bool) -> Self {
        self.stream_markdown = enabled;
        self
    }

    pub fn gateway_enabled(mut self, enabled: bool) -> Self {
        self.gateway_enabled = enabled;
        self
    }
}

/// Programmatic pVisor driver assembly configuration.
#[derive(Debug, Clone, Default)]
pub struct PVisorConfig {
    pub gateway: Option<GatewayDriverConfig>,
    pub network: NetworkDriverConfig,
    pub overlay: OverlayHint,
}

impl PVisorConfig {
    pub fn with_gateway(mut self, gateway: GatewayDriverConfig) -> Self {
        self.gateway = Some(gateway);
        self
    }

    pub fn with_overlay(mut self, overlay: OverlayHint) -> Self {
        self.overlay = overlay;
        self
    }

    pub fn with_network(mut self, network: NetworkDriverConfig) -> Self {
        self.network = network;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_config_toml_roundtrip() {
        let config: RunConfig = toml::from_str(
            r#"
[run]
executor = "container"
command = ["codex"]

[container]
runtime = "podman"
image = "example/agent:latest"
network = "none"

[overlayfs]
base = "/tmp/lower"

[overlaynet]
mode = "proxy"
policy = "allowlist"

[[overlaynet.rules]]
host = "api.openai.com"
ports = [443]
transports = ["tcp_tunnel"]

[[overlaynet.deny]]
host = "169.254.0.0/16"

[[overlaynet.limits]]
bytes_per_second = 1250000

[gateway]
mode = "capture"

[[gateway.routes]]
name = "openai"
upstream = "https://api.openai.com/v1"
"#,
        )
        .unwrap();
        assert_eq!(
            config
                .overlayfs
                .as_ref()
                .and_then(|overlay| overlay.base.as_deref()),
            Some(Path::new("/tmp/lower"))
        );
        assert_eq!(config.run.executor, RunExecutorKind::Container);
        assert_eq!(config.container.runtime, Path::new("podman"));
        assert_eq!(config.container.image, "example/agent:latest");
        assert_eq!(config.container.network, ContainerNetwork::None);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.overlaynet.rules.len(), 1);
        assert_eq!(config.overlaynet.rules[0].ports, [443]);
        assert_eq!(config.overlaynet.deny.len(), 1);
        assert_eq!(config.overlaynet.limits[0].bytes_per_second, 1_250_000);
        assert_eq!(config.gateway.routes.len(), 1);
        assert_eq!(config.run.command, ["codex"]);
    }

    #[test]
    fn vm_config_toml_roundtrip() {
        let config: RunConfig = toml::from_str(
            r#"
[run]
executor = "vm"
command = ["agent"]

[vm]
rootfs = "/opt/rootfs"
library_dir = "/opt/libkrun/lib"
memory_mib = 4096
cpus = 4
"#,
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/opt/rootfs")));
        assert_eq!(
            config.vm.library_dir.as_deref(),
            Some(Path::new("/opt/libkrun/lib"))
        );
        assert_eq!(config.vm.memory_mib, 4096);
        assert_eq!(config.vm.cpus, 4);
        let encoded = toml::to_string_pretty(&config).unwrap();
        let decoded: RunConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.vm, config.vm);
    }

    #[test]
    fn overlaynet_defaults_to_auto_for_executor_specific_selection() {
        let config = RunConfig::default();
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Auto);
        assert_eq!(PVisorConfig::default().network.mode, OverlayNetMode::Auto);
    }

    #[test]
    fn legacy_kvm_config_deserializes_as_vm() {
        let config: RunConfig = toml::from_str(
            r#"
[run]
executor = "kvm"
command = ["agent"]

[kvm]
rootfs = "/opt/rootfs"
"#,
        )
        .unwrap();
        assert_eq!(config.run.executor, RunExecutorKind::Vm);
        assert_eq!(config.vm.rootfs.as_deref(), Some(Path::new("/opt/rootfs")));
    }
}
