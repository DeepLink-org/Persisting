//! Proxy configuration (TOML): `models[]` entries + optional `forward` to another model name.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

pub use crate::protocol::ProtocolKind;
pub use crate::provider::ProviderKind;
pub use persisting_overlaynet::{NetworkConfig, NetworkMode};

/// Top-level config for capture proxy / daemon.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyConfig {
    pub listen: String,
    /// Admin API for `capture status` (default `127.0.0.1:9876`).
    #[serde(default = "default_admin_listen")]
    pub admin_listen: String,
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    #[serde(default = "default_session_header")]
    pub session_header: String,
    /// What to persist in trajectory: `summary` (metadata), `dialogue` (default, user/assistant text), `full` (raw bodies).
    #[serde(default)]
    pub capture_level: CaptureLevel,
    /// Log every proxied / captured HTTP request to stderr and `{storage}/.capture/debug.log`.
    #[serde(default)]
    pub debug: bool,
    /// Harbor-aligned egress policy for forward-proxy traffic (`CONNECT` + absolute-URI).
    #[serde(default)]
    pub network: NetworkConfig,
    /// Optional embedded OverlayFS mount for the Attempt (consumed by pVisor).
    #[serde(default)]
    pub overlay: OverlayConfig,
    pub models: Vec<ModelRoute>,
}

/// Filesystem overlay settings (same capture TOML; applied by pVisor).
///
/// Model: **target** (read-only base / apply destination) + **staging** (upper
/// holds deltas). The Agent sees `merged`; changes do **not** touch `target`
/// until an explicit runtime overlay is applied.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverlayConfig {
    /// When true, pVisor mounts its embedded OverlayFS for the Attempt.
    #[serde(default)]
    pub enabled: bool,
    /// Target filesystem: primary lower layer and destination for `apply`.
    /// Prefer this over listing the same path in `lower_dirs`.
    #[serde(default)]
    pub target: Option<String>,
    /// Read-only compose layers stacked above `target`, highest priority first.
    #[serde(default)]
    pub lower_dirs: Vec<String>,
    /// Root for staging (`upper` / `work` / `merged`). Default:
    /// `{capture_storage}/.overlay/{session_id}/`.
    #[serde(default)]
    pub stage_dir: Option<String>,
    /// Writable upper backend. `directory` is the default; `jujutsu` adds
    /// named persistent forks in a shared repository.
    #[serde(default)]
    pub backend: OverlayBackend,
    /// Shared Jujutsu store. All named workspaces use the same object store and
    /// operation log (overrides `{storage}/.overlay/jujutsu`).
    #[serde(default)]
    pub jujutsu_store_path: Option<String>,
    /// Jujutsu workspace/fork name (defaults to the pVisor session id).
    #[serde(default)]
    pub jujutsu_workspace: Option<String>,
    /// Writable upper directory (overrides `{stage_dir}/upper` when set).
    #[serde(default)]
    pub upper_dir: Option<String>,
    /// Overlay work directory (overrides `{stage_dir}/work` when set).
    #[serde(default)]
    pub work_dir: Option<String>,
    /// Merged mount point (overrides `{stage_dir}/merged` when set).
    #[serde(default)]
    pub merged_dir: Option<String>,
    /// If true, apply staging onto `target` automatically when the Attempt ends.
    /// Default false — review then `pvisor apply` or `pvisor drop`.
    #[serde(default)]
    pub auto_apply: bool,
    /// If true, discard staging automatically when the Attempt ends.
    #[serde(default)]
    pub auto_discard: bool,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlayBackend {
    #[default]
    Directory,
    Jujutsu,
}

fn default_admin_listen() -> String {
    "127.0.0.1:9876".to_string()
}

fn default_agent_id() -> String {
    "default".to_string()
}

fn default_session_header() -> String {
    "x-persisting-session-id".to_string()
}

/// Controls how much request/response content is written to trajectory records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureLevel {
    /// Model, path, byte counts — no message text.
    Summary,
    /// User / assistant dialogue text (default).
    #[default]
    Dialogue,
    /// Full parsed JSON bodies in `payload.body`.
    Full,
}

impl CaptureLevel {
    pub fn includes_user_text(self) -> bool {
        !matches!(self, Self::Summary)
    }

    pub fn includes_assistant_text(self) -> bool {
        !matches!(self, Self::Summary)
    }

    pub fn includes_full_body(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    /// Match pattern (exact, `prefix*`, `*suffix`, `*`) or target model id.
    pub name: String,
    /// `openai` | `anthropic` | `gemini` | `vertex` | `bedrock` | `azure` | `copilot` | `custom`
    #[serde(default)]
    pub provider: Option<String>,
    /// OpenAI-compatible upstream base (include API prefix, e.g. `https://api.deepseek.com/v1`).
    #[serde(default)]
    pub upstream: Option<String>,
    /// Anthropic-compatible upstream (e.g. `https://api.deepseek.com/anthropic/v1`). Falls back to `upstream`.
    #[serde(default)]
    pub upstream_anthropic: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Forward to another `models[].name` (exact id): use its upstream and rewrite request `model`.
    #[serde(default)]
    pub forward: Option<String>,
}

impl ProxyConfig {
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        let cfg: Self = toml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn from_toml_file(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_toml_str(&s)
    }

    /// Load a TOML proxy configuration.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("toml") | None => Self::from_toml_file(path),
            Some(ext) => anyhow::bail!("unsupported proxy config extension `.{ext}` (use .toml)"),
        }
    }

    pub fn to_toml_string(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Validate model entries, `forward` references, network policy, and duplicate names.
    pub fn validate(&self) -> anyhow::Result<()> {
        persisting_overlaynet::policy::validate_network_config(&self.network)?;
        if self.overlay.auto_apply && self.overlay.auto_discard {
            anyhow::bail!("overlay auto_apply and auto_discard are mutually exclusive");
        }
        match self.overlay.backend {
            OverlayBackend::Directory => {
                if self.overlay.jujutsu_store_path.is_some()
                    || self.overlay.jujutsu_workspace.is_some()
                {
                    anyhow::bail!("overlay backend `directory` cannot use Jujutsu options");
                }
            }
            OverlayBackend::Jujutsu => {
                if self.overlay.upper_dir.is_some() || self.overlay.work_dir.is_some() {
                    anyhow::bail!("overlay backend `jujutsu` cannot use directory upper options");
                }
            }
        }
        let mut seen = HashSet::new();
        for route in &self.models {
            if !seen.insert(route.name.clone()) {
                anyhow::bail!("duplicate models[].name `{}`", route.name);
            }
            match (&route.forward, &route.upstream) {
                (Some(fwd), None) => {
                    if fwd == &route.name {
                        anyhow::bail!(
                            "models[].forward must reference another entry, not `{}`",
                            route.name
                        );
                    }
                }
                (None, None) => {
                    anyhow::bail!(
                        "models[] entry `{}` needs `upstream` or `forward`",
                        route.name
                    );
                }
                (Some(_), Some(_)) => {
                    anyhow::bail!(
                        "models[] entry `{}` cannot set both `forward` and `upstream`",
                        route.name
                    );
                }
                (None, Some(_)) => {}
            }
        }
        for route in &self.models {
            let Some(fwd) = &route.forward else {
                continue;
            };
            let target = self
                .models
                .iter()
                .find(|r| r.name == *fwd)
                .ok_or_else(|| anyhow::anyhow!("forward target `{fwd}` not found"))?;
            if target.forward.is_some() {
                anyhow::bail!("forward target `{fwd}` cannot forward again");
            }
            if target.upstream.is_none() {
                anyhow::bail!("forward target `{fwd}` missing upstream");
            }
        }
        Ok(())
    }
}

impl ModelRoute {
    pub fn provider_kind(&self) -> ProviderKind {
        self.provider
            .as_deref()
            .map(ProviderKind::parse)
            .unwrap_or(ProviderKind::OpenAi)
    }

    /// Provider used for indexing / cost when protocol selects an Anthropic upstream.
    pub fn effective_provider(&self, protocol: ProtocolKind) -> ProviderKind {
        if protocol == ProtocolKind::Messages && self.upstream_anthropic.is_some() {
            return ProviderKind::Anthropic;
        }
        self.provider_kind()
    }

    fn effective_upstream_base(&self, protocol: ProtocolKind) -> anyhow::Result<&str> {
        if protocol == ProtocolKind::Messages {
            if let Some(ref u) = self.upstream_anthropic {
                return Ok(u.as_str());
            }
        }
        self.upstream
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("model `{}` has no upstream", self.name))
    }

    pub fn resolve_upstream_url(
        &self,
        incoming_path: &str,
        protocol: ProtocolKind,
    ) -> anyhow::Result<Url> {
        let base_str = self.effective_upstream_base(protocol)?;
        let mut base = Url::parse(base_str)
            .map_err(|e| anyhow::anyhow!("invalid upstream for model {}: {e}", self.name))?;
        let api_prefix = detect_incoming_api_prefix(incoming_path).to_string();
        let suffix = strip_incoming_api_prefix(incoming_path, &api_prefix);
        let base_path = base.path().trim_end_matches('/');

        let final_path = if base_path.is_empty() || base_path == "/" {
            join_api_path(&api_prefix, &suffix)
        } else if base_includes_api_prefix(base_path, &api_prefix) {
            join_api_path(base_path, &suffix)
        } else {
            join_api_path(&format!("{base_path}{api_prefix}"), &suffix)
        };

        base.set_path(&final_path);
        Ok(base)
    }

    pub fn api_key_value(&self) -> anyhow::Result<Option<String>> {
        if let Some(ref k) = self.api_key {
            return Ok(Some(k.clone()));
        }
        if let Some(ref env) = self.api_key_env {
            return Ok(lookup_env_var(env));
        }
        Ok(None)
    }
}

/// Read an API-key env var plus known Claude Code / provider aliases.
pub fn lookup_env_var(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(name) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    for alias in api_key_env_aliases(name) {
        if let Ok(v) = std::env::var(alias) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

pub fn api_key_env_aliases(primary: &str) -> &'static [&'static str] {
    match primary {
        "DEEPSEEK_API_KEY" => &["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"],
        "ANTHROPIC_API_KEY" => &["ANTHROPIC_AUTH_TOKEN", "DEEPSEEK_API_KEY"],
        "ANTHROPIC_AUTH_TOKEN" => &["ANTHROPIC_API_KEY", "DEEPSEEK_API_KEY"],
        "OPENAI_API_KEY" => &["ANTHROPIC_AUTH_TOKEN"],
        _ => &[],
    }
}

/// Detect API version prefix from the client request path (`/v1/messages` → `/v1`).
fn detect_incoming_api_prefix(incoming_path: &str) -> &'static str {
    let incoming = incoming_path.trim_start_matches('/');
    if incoming.starts_with("v1beta/") || incoming == "v1beta" {
        "/v1beta"
    } else {
        "/v1"
    }
}

fn base_includes_api_prefix(base_path: &str, api_prefix: &str) -> bool {
    let base = base_path.trim_end_matches('/');
    let prefix = api_prefix.trim_start_matches('/').trim_end_matches('/');
    if base.is_empty() {
        return false;
    }
    base == prefix || base.ends_with(&format!("/{prefix}"))
}

/// Strip the incoming API version prefix (e.g. `/v1/messages` → `messages`).
fn strip_incoming_api_prefix(incoming_path: &str, api_prefix: &str) -> String {
    let incoming = incoming_path.trim_start_matches('/');
    let prefix = api_prefix.trim_start_matches('/').trim_end_matches('/');
    if incoming == prefix {
        return String::new();
    }
    if let Some(rest) = incoming.strip_prefix(&format!("{prefix}/")) {
        return rest.to_string();
    }
    incoming.to_string()
}

fn join_api_path(prefix: &str, suffix: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    if suffix.is_empty() {
        if prefix.is_empty() {
            "/".to_string()
        } else {
            prefix.to_string()
        }
    } else if prefix.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{prefix}/{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ProtocolKind;

    fn route(upstream: &str) -> ModelRoute {
        route_with_anthropic(upstream, None)
    }

    fn route_with_anthropic(upstream: &str, upstream_anthropic: Option<&str>) -> ModelRoute {
        ModelRoute {
            name: "test".into(),
            provider: None,
            upstream: Some(upstream.into()),
            upstream_anthropic: upstream_anthropic.map(str::to_string),
            api_key_env: None,
            api_key: None,
            forward: None,
        }
    }

    #[test]
    fn upstream_url_strips_duplicate_v1() {
        let r = route("http://127.0.0.1:19080/v1");
        let url = r
            .resolve_upstream_url("/v1/messages", ProtocolKind::Messages)
            .unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:19080/v1/messages");

        let url = r
            .resolve_upstream_url("/v1/chat/completions", ProtocolKind::ChatCompletions)
            .unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:19080/v1/chat/completions");
    }

    #[test]
    fn upstream_url_anthropic_host_with_v1_in_upstream() {
        let r = route("https://api.anthropic.com/v1");
        let url = r
            .resolve_upstream_url("/v1/messages", ProtocolKind::Messages)
            .unwrap();
        assert_eq!(url.as_str(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn upstream_url_when_base_already_has_prefix() {
        let r = route("https://api.openai.com/v1");
        let url = r
            .resolve_upstream_url("/v1/chat/completions", ProtocolKind::ChatCompletions)
            .unwrap();
        assert_eq!(url.as_str(), "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn upstream_url_v1beta_prefix() {
        let r = route("https://generativelanguage.googleapis.com/v1beta");
        let url = r
            .resolve_upstream_url(
                "/v1beta/models/gemini-pro:generateContent",
                ProtocolKind::Unknown,
            )
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-pro:generateContent"
        );
    }

    #[test]
    fn upstream_url_deepseek_anthropic_base() {
        let r = route_with_anthropic(
            "https://api.deepseek.com/v1",
            Some("https://api.deepseek.com/anthropic/v1"),
        );
        let url = r
            .resolve_upstream_url("/v1/messages", ProtocolKind::Messages)
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        let url = r
            .resolve_upstream_url("/v1/chat/completions", ProtocolKind::ChatCompletions)
            .unwrap();
        assert_eq!(url.as_str(), "https://api.deepseek.com/v1/chat/completions");
    }

    #[test]
    fn effective_provider_anthropic_when_dual_upstream() {
        let r = route_with_anthropic(
            "https://api.deepseek.com/v1",
            Some("https://api.deepseek.com/anthropic/v1"),
        );
        assert_eq!(
            r.effective_provider(ProtocolKind::Messages),
            ProviderKind::Anthropic
        );
        assert_eq!(
            r.effective_provider(ProtocolKind::ChatCompletions),
            ProviderKind::OpenAi
        );
    }

    #[test]
    fn toml_roundtrip_and_deny_unknown_field() {
        let src = r#"
listen = "127.0.0.1:1"

[[models]]
name = "*"
upstream = "http://example.com/v1"
unknown_field = true
"#;
        assert!(ProxyConfig::from_toml_str(src).is_err());

        let cfg = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[[models]]
name = "*"
upstream = "http://example.com/v1"
"#,
        )
        .unwrap();
        let again = ProxyConfig::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert_eq!(cfg.listen, again.listen);
    }

    #[test]
    fn config_rejects_allow_entries_without_allowlist_mode() {
        let error = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[network]
allowed_hosts = ["api.example.com"]

[[models]]
name = "*"
upstream = "http://example.com/v1"
"#,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("network allow entries require mode"));
    }

    #[test]
    fn config_rejects_invalid_bandwidth_limits_at_load_time() {
        let error = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:1"

[network]
mode = "public"

[[network.limits]]
bytes_per_second = 0

[[models]]
name = "*"
upstream = "http://example.com/v1"
"#,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("network bandwidth limit must be greater than zero"));
    }
}
