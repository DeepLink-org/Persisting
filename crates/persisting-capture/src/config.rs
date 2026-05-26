//! Proxy configuration: `models[]` entries + optional `forward` to another model name.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use url::Url;

use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;

/// Top-level config for capture proxy / daemon.
#[derive(Debug, Clone, Deserialize)]
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
    pub models: Vec<ModelRoute>,
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

#[derive(Debug, Clone, Deserialize)]
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
    /// Legacy: prefer putting the API prefix in `upstream` instead.
    #[serde(default)]
    pub(crate) path_prefix: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Forward to another `models[].name` (exact id): use its upstream and rewrite request `model`.
    #[serde(default)]
    pub forward: Option<String>,
}

impl ProxyConfig {
    pub fn from_yaml_str(s: &str) -> anyhow::Result<Self> {
        let cfg: Self = serde_yaml::from_str(s)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn from_yaml_file(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path)?;
        Self::from_yaml_str(&s)
    }

    /// Validate model entries, `forward` references, and duplicate names.
    pub fn validate(&self) -> anyhow::Result<()> {
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

    fn effective_upstream_base<'a>(&'a self, protocol: ProtocolKind) -> anyhow::Result<&'a str> {
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
        let api_prefix = self.effective_api_prefix(incoming_path);
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

    fn effective_api_prefix(&self, incoming_path: &str) -> String {
        if let Some(ref prefix) = self.path_prefix {
            return prefix.clone();
        }
        detect_incoming_api_prefix(incoming_path).to_string()
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
    } else if incoming.starts_with("v1/") || incoming == "v1" {
        "/v1"
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
            path_prefix: None,
            api_key_env: None,
            api_key: None,
            forward: None,
        }
    }

    fn legacy_route(upstream: &str, path_prefix: &str) -> ModelRoute {
        ModelRoute {
            name: "test".into(),
            provider: None,
            upstream: Some(upstream.into()),
            upstream_anthropic: None,
            path_prefix: Some(path_prefix.into()),
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
    fn legacy_path_prefix_host_only_upstream() {
        let r = legacy_route("https://api.openai.com", "/v1");
        let url = r
            .resolve_upstream_url("/v1/chat/completions", ProtocolKind::ChatCompletions)
            .unwrap();
        assert_eq!(url.as_str(), "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn legacy_path_prefix_deepseek_anthropic_split() {
        let r = ModelRoute {
            name: "test".into(),
            provider: None,
            upstream: Some("https://api.deepseek.com".into()),
            upstream_anthropic: Some("https://api.deepseek.com/anthropic".into()),
            path_prefix: Some("/v1".into()),
            api_key_env: None,
            api_key: None,
            forward: None,
        };
        let url = r
            .resolve_upstream_url("/v1/messages", ProtocolKind::Messages)
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
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
}
