//! Shared proxy config flags and environment variables for `gateway`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use persisting_gateway::config::{
    env, materialize_proxy_config, parse_capture_level, resolve_proxy_config, CaptureLevel,
    ProxyConfig, ProxyConfigInput, ProxyConfigOverrides,
};

/// Proxy settings overridable via CLI flags and environment variables.
#[derive(Debug, Clone, Default, Args)]
pub struct ProxyConfigOverrideArgs {
    /// Proxy listen address (overrides TOML `listen`).
    #[arg(long, env = env::LISTEN, value_name = "ADDR")]
    pub listen: Option<String>,
    /// Admin API listen address (overrides TOML `admin_listen`).
    #[arg(long, env = env::ADMIN_LISTEN, value_name = "ADDR")]
    pub admin_listen: Option<String>,
    /// Trajectory agent id segment (overrides TOML `agent_id`).
    #[arg(long, env = env::AGENT_ID, value_name = "SEG")]
    pub agent_id: Option<String>,
    /// Session id HTTP header name (overrides TOML `session_header`).
    #[arg(long, env = env::SESSION_HEADER, value_name = "HEADER")]
    pub session_header: Option<String>,
    /// Capture granularity: `summary`, `dialogue`, or `full`.
    #[arg(long, env = env::CAPTURE_LEVEL, value_name = "LEVEL")]
    pub capture_level: Option<String>,
    /// Model routes as JSON array (overrides all `[[models]]` entries).
    #[arg(long, env = env::MODELS_JSON, value_name = "JSON")]
    pub models_json: Option<String>,
    /// Model routes as TOML `[[models]]` table(s) (overrides all routes).
    #[arg(long, env = env::MODELS_TOML, value_name = "TOML")]
    pub models_toml: Option<String>,
    /// Model route name to create or patch (default: `*`).
    #[arg(long, env = env::MODEL, value_name = "NAME")]
    pub model: Option<String>,
    /// Upstream OpenAI-compatible base URL for `--model` (e.g. `https://api.deepseek.com/v1`).
    #[arg(long, env = env::UPSTREAM, value_name = "URL")]
    pub upstream: Option<String>,
    /// Anthropic-compatible upstream for `/v1/messages`.
    #[arg(long, env = env::UPSTREAM_ANTHROPIC, value_name = "URL")]
    pub upstream_anthropic: Option<String>,
    /// Provider label for the model route: `openai`, `anthropic`, …
    #[arg(long, env = env::PROVIDER, value_name = "NAME")]
    pub provider: Option<String>,
    /// Env var holding the upstream API key for this route.
    #[arg(long, env = env::API_KEY_ENV, value_name = "VAR")]
    pub api_key_env: Option<String>,
    /// Inline upstream API key (prefer `--api-key-env` in scripts).
    #[arg(long, env = env::API_KEY, value_name = "KEY")]
    pub api_key: Option<String>,
    /// Forward client model to another configured route name.
    #[arg(long, env = env::FORWARD, value_name = "NAME")]
    pub forward: Option<String>,
}

impl ProxyConfigOverrideArgs {
    pub fn to_overrides(&self) -> Result<ProxyConfigOverrides> {
        let capture_level = match &self.capture_level {
            Some(s) => Some(parse_capture_level(s)?),
            None => None,
        };
        Ok(ProxyConfigOverrides {
            listen: self.listen.clone(),
            admin_listen: self.admin_listen.clone(),
            agent_id: self.agent_id.clone(),
            session_header: self.session_header.clone(),
            capture_level,
            debug: None,
            models_json: self.models_json.clone(),
            models_toml: self.models_toml.clone(),
            model_name: self.model.clone(),
            upstream: self.upstream.clone(),
            upstream_anthropic: self.upstream_anthropic.clone(),
            provider: self.provider.clone(),
            api_key_env: self.api_key_env.clone(),
            api_key: self.api_key.clone(),
            forward: self.forward.clone(),
        })
    }
}

/// Optional proxy TOML path plus CLI/env overrides.
#[derive(Debug, Clone, Default, Args)]
pub struct ProxyConfigArgs {
    /// Proxy config TOML (`listen`, `models`, …). Optional when set via env/flags.
    #[arg(
        long,
        short = 'c',
        value_name = "FILE",
        env = env::CONFIG_FILE,
        conflicts_with = "config_toml"
    )]
    pub config: Option<PathBuf>,
    /// Full proxy config as inline TOML (alternative to `-c`; supports every TOML field).
    #[arg(long, env = env::CONFIG_TOML, value_name = "TOML", conflicts_with = "config")]
    pub config_toml: Option<String>,
    #[command(flatten)]
    pub overrides: ProxyConfigOverrideArgs,
}

impl ProxyConfigArgs {
    pub fn input(&self) -> Result<ProxyConfigInput> {
        Ok(ProxyConfigInput {
            config_file: self.config.clone(),
            config_toml: self.config_toml.clone(),
            overrides: self.overrides.to_overrides()?,
        })
    }

    pub fn resolve(&self) -> Result<ProxyConfig> {
        resolve_proxy_config(&self.input()?)
    }

    pub fn resolve_with_debug(&self, cli_debug: bool) -> Result<ProxyConfig> {
        let mut input = self.input()?;
        if cli_debug {
            input.overrides.debug = Some(true);
        }
        resolve_proxy_config(&input)
    }
}

/// Resolved config and on-disk path (explicit `-c` or materialized `{storage}/proxy.toml`).
pub struct ResolvedProxyConfig {
    pub config: ProxyConfig,
    pub config_path: PathBuf,
}

impl ProxyConfigArgs {
    pub fn materialize(
        &self,
        storage: &std::path::Path,
        cli_debug: bool,
    ) -> Result<ResolvedProxyConfig> {
        let mut input = self.input()?;
        if cli_debug {
            input.overrides.debug = Some(true);
        }
        let (config, config_path) =
            materialize_proxy_config(storage, &input).context("resolve proxy config")?;
        Ok(ResolvedProxyConfig {
            config,
            config_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_args_map_capture_level() {
        let args = ProxyConfigOverrideArgs {
            capture_level: Some("full".into()),
            upstream: Some("http://127.0.0.1:1/v1".into()),
            ..Default::default()
        };
        let o = args.to_overrides().unwrap();
        assert_eq!(o.capture_level, Some(CaptureLevel::Full));
    }
}
