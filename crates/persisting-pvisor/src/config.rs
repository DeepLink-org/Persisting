//! Canonical pVisor Run configuration.
//!
//! TOML and the `pvisor run` command line both resolve into [`RunConfig`].
//! Runtime drivers only consume the resolved value and do not read config
//! files themselves.

use std::path::{Path, PathBuf};

use persisting_gateway::config::{CaptureLevel, ModelRoute, ProxyConfig};
use serde::{Deserialize, Serialize};

use crate::runtime::OverlayHint;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RunConfig {
    pub run: RunSettings,
    pub overlayfs: OverlayFsSettings,
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
    pub workspace: Option<PathBuf>,
    pub agent: String,
    pub timeout_ms: Option<u64>,
    pub stdio: RunStdio,
    pub policy: RunPolicy,
    pub command: Vec<String>,
}

impl Default for RunSettings {
    fn default() -> Self {
        Self {
            workspace: None,
            agent: "agent".into(),
            timeout_ms: None,
            stdio: RunStdio::Inherit,
            policy: RunPolicy::Observe,
            command: Vec::new(),
        }
    }
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
    pub mode: OverlayFsMode,
    pub target: Option<PathBuf>,
    pub lower: Vec<PathBuf>,
    pub backend: OverlayFsBackend,
    pub jujutsu_store: Option<PathBuf>,
    pub jujutsu_workspace: Option<String>,
    pub commit: OverlayFsCommit,
}

impl Default for OverlayFsSettings {
    fn default() -> Self {
        Self {
            mode: OverlayFsMode::Host,
            target: None,
            lower: Vec::new(),
            backend: OverlayFsBackend::Directory,
            jujutsu_store: None,
            jujutsu_workspace: None,
            commit: OverlayFsCommit::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayFsMode {
    #[default]
    Host,
    Overlay,
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
}

impl Default for OverlayNetSettings {
    fn default() -> Self {
        Self {
            mode: OverlayNetMode::Off,
            listen: "127.0.0.1:19081".into(),
            policy: OverlayNetPolicy::Public,
            allow: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayNetMode {
    #[default]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_config_toml_roundtrip() {
        let config: RunConfig = toml::from_str(
            r#"
[run]
workspace = "/tmp/run"
command = ["codex"]

[overlayfs]
mode = "overlay"
target = "/tmp/lower"

[overlaynet]
mode = "proxy"
policy = "allowlist"
allow = ["api.openai.com"]

[gateway]
mode = "capture"

[[gateway.routes]]
name = "openai"
upstream = "https://api.openai.com/v1"
"#,
        )
        .unwrap();
        assert_eq!(config.overlayfs.mode, OverlayFsMode::Overlay);
        assert_eq!(config.overlaynet.mode, OverlayNetMode::Proxy);
        assert_eq!(config.gateway.routes.len(), 1);
        assert_eq!(config.run.command, ["codex"]);
    }
}
