use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::warn;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_admin_listen")]
    pub admin_listen: String,
    #[serde(default = "default_store_dir")]
    pub store_dir: String,
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    #[serde(default = "default_session_header")]
    pub session_header: String,
    #[serde(default)]
    pub session_header_aliases: Vec<String>,
    #[serde(default = "default_session_id")]
    pub default_session_id: String,
    #[serde(default = "default_preserve_raw")]
    pub preserve_raw: bool,
    #[serde(default = "default_base_session_path")]
    pub base_session_path: String,
    #[serde(default)]
    pub models: Vec<ModelRoute>,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub export: ExportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRoute {
    pub name: String,
    pub provider: String,
    pub upstream_base_url: String,
    pub api_key: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_authoritative")]
    pub authoritative: String,
    #[serde(default = "default_also_md")]
    pub also: Vec<String>,
    #[serde(default)]
    pub json_cache: JsonCacheConfig,
    #[serde(default)]
    pub lance: LanceStorageConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanceStorageConfig {
    #[serde(default, alias = "uri")]
    pub db_uri: String,
    #[serde(default = "default_lance_table_name")]
    pub table_name: String,
    #[serde(default = "default_lance_backend")]
    pub backend: String,
    #[serde(default = "default_lance_mode")]
    pub mode: String,
    #[serde(default = "default_fail_open")]
    pub fail_open: bool,
    #[serde(default = "default_dead_letter_path")]
    pub dead_letter_path: String,
    #[serde(default)]
    pub s3: Option<LanceS3Config>,
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    #[serde(default)]
    pub flush_interval_ms: u64,
    #[serde(default)]
    pub write_timeout_ms: u64,
    #[serde(default)]
    pub async_writer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanceS3Config {
    pub region: String,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// When unset, defaults to true if `endpoint` uses `http://`.
    #[serde(default)]
    pub allow_http: Option<bool>,
}

impl LanceS3Config {
    fn allow_http_enabled(&self) -> bool {
        if let Some(allow_http) = self.allow_http {
            return allow_http;
        }
        self.endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.to_ascii_lowercase().starts_with("http://"))
    }

    pub fn to_storage_options(&self) -> HashMap<String, String> {
        let mut opts = HashMap::new();
        if !self.region.is_empty() {
            opts.insert("aws_region".to_string(), self.region.clone());
        }
        if let Some(endpoint) = &self.endpoint {
            if !endpoint.is_empty() {
                opts.insert("aws_endpoint".to_string(), endpoint.clone());
            }
        }
        if self.allow_http_enabled() {
            opts.insert("allow_http".to_string(), "true".to_string());
        }
        opts
    }
}

impl LanceStorageConfig {
    pub fn is_s3(&self) -> bool {
        self.db_uri.starts_with("s3://")
    }

    pub fn storage_options(&self) -> HashMap<String, String> {
        let mut opts = self
            .s3
            .as_ref()
            .map(LanceS3Config::to_storage_options)
            .unwrap_or_default();
        if !opts.contains_key("aws_region")
            && let Ok(region) = std::env::var("AWS_REGION")
            && !region.is_empty()
        {
            opts.insert("aws_region".to_string(), region);
        }
        opts
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct JsonCacheConfig {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConfig {
    #[serde(default = "default_max_steps")]
    pub max_steps_per_session: u64,
    #[serde(default = "default_messages_drift")]
    pub messages_drift: String,
    #[serde(default)]
    pub defaults: ExportDefaults,
    #[serde(default)]
    pub session_metadata: Map<String, Value>,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            max_steps_per_session: default_max_steps(),
            messages_drift: default_messages_drift(),
            defaults: ExportDefaults::default(),
            session_metadata: Map::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportDefaults {
    #[serde(default = "default_env_name")]
    pub env_name: String,
    #[serde(default = "default_job_id")]
    pub job_id: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub step_reward: f64,
    #[serde(default)]
    pub reward: f64,
    #[serde(default = "default_env_state")]
    pub env_state: Value,
    #[serde(default)]
    pub is_terminal: bool,
    #[serde(default = "default_is_trainable")]
    pub is_trainable: bool,
}

impl Default for ExportDefaults {
    fn default() -> Self {
        Self {
            env_name: default_env_name(),
            job_id: default_job_id(),
            group_id: String::new(),
            step_reward: 0.0,
            reward: 0.0,
            env_state: default_env_state(),
            is_terminal: false,
            is_trainable: true,
        }
    }
}

impl StorageConfig {
    pub fn lance_enabled(&self) -> bool {
        self.authoritative == "lance" || self.also.iter().any(|s| s == "lance")
    }

    pub fn json_cache_enabled(&self) -> bool {
        self.authoritative == "lance"
            && (self.also.iter().any(|s| s == "json_cache") || self.json_cache.enabled)
    }

    pub fn validate(&self) -> Result<()> {
        if !VALID_AUTHORITATIVE.contains(&self.authoritative.as_str()) {
            bail!(
                "storage.authoritative must be one of: {}",
                VALID_AUTHORITATIVE.join(", ")
            );
        }

        for token in &self.also {
            if !VALID_ALSO_TOKENS.contains(&token.as_str()) {
                bail!(
                    "storage.also contains unknown token '{token}'; allowed: {}",
                    VALID_ALSO_TOKENS.join(", ")
                );
            }
        }

        if self.authoritative != "lance"
            && (self.also.iter().any(|s| s == "json_cache") || self.json_cache.enabled)
        {
            bail!("storage.json_cache is only valid when storage.authoritative = \"lance\"");
        }

        if self.lance_enabled() {
            if self.lance.db_uri.trim().is_empty() {
                bail!("storage.lance.db_uri is required when Lance sink is enabled");
            }
            if self.lance.backend != "lance" {
                bail!(
                    "storage.lance.backend must be \"lance\" in P0 (got \"{}\")",
                    self.lance.backend
                );
            }
            if !VALID_LANCE_MODES.contains(&self.lance.mode.as_str()) {
                bail!(
                    "storage.lance.mode must be one of: {}",
                    VALID_LANCE_MODES.join(", ")
                );
            }
            if self.lance.is_s3() {
                let has_region = self
                    .lance
                    .s3
                    .as_ref()
                    .is_some_and(|s| !s.region.trim().is_empty())
                    || std::env::var("AWS_REGION")
                        .map(|r| !r.trim().is_empty())
                        .unwrap_or(false);
                if !has_region {
                    bail!(
                        "storage.lance.s3.region is required when db_uri uses s3:// (unless AWS_REGION is set)"
                    );
                }
            }
        }

        if self.authoritative == "lance" && self.also.iter().any(|s| s == "lance") {
            warn!(
                "storage.also contains redundant \"lance\" when authoritative is already \"lance\""
            );
        }

        Ok(())
    }
}

impl ProxyConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed reading config file: {}", path.display()))?;
        let mut parsed: ProxyConfig =
            toml::from_str(&raw).with_context(|| "failed parsing proxy.toml".to_string())?;
        if let Ok(uri) =
            std::env::var("DLCAPT_LANCE_DB_URI").or_else(|_| std::env::var("CAPTURE_LANCE_URI"))
        {
            if !uri.trim().is_empty() {
                parsed.storage.lance.db_uri = uri;
            }
        }
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<()> {
        if self.models.is_empty() {
            bail!("proxy config must contain at least one [[models]] route");
        }

        for model in &self.models {
            if model.name.trim().is_empty() {
                bail!("model route name cannot be empty");
            }
            if model.provider.trim().is_empty() {
                bail!("model provider cannot be empty");
            }
            if model.upstream_base_url.trim().is_empty() {
                bail!("model upstream_base_url cannot be empty");
            }
        }

        self.storage.validate()?;

        Ok(())
    }

    pub fn session_settings(&self) -> crate::session::SessionIdSettings {
        crate::session::SessionIdSettings {
            default_session_id: self.default_session_id.clone(),
            preserve_raw: self.preserve_raw,
            session_header: self.session_header.clone(),
            session_header_aliases: self.session_header_aliases.clone(),
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:19081".to_string()
}

fn default_admin_listen() -> String {
    "127.0.0.1:19082".to_string()
}

fn default_store_dir() -> String {
    "store".to_string()
}

fn default_agent_id() -> String {
    "openclaw".to_string()
}

fn default_session_header() -> String {
    "x-persisting-session-id".to_string()
}

fn default_session_id() -> String {
    "default".to_string()
}

fn default_preserve_raw() -> bool {
    false
}

fn default_base_session_path() -> String {
    "/v1/sessions".to_string()
}

fn default_authoritative() -> String {
    "json_file".to_string()
}

fn default_also_md() -> Vec<String> {
    vec!["md".to_string()]
}

fn default_max_steps() -> u64 {
    1000
}

fn default_messages_drift() -> String {
    "trust_request".to_string()
}

fn default_env_name() -> String {
    "openclaw".to_string()
}

fn default_job_id() -> String {
    "dlcapt".to_string()
}

fn default_env_state() -> Value {
    Value::Object(Map::new())
}

fn default_is_trainable() -> bool {
    true
}

const VALID_AUTHORITATIVE: &[&str] = &["lance", "json_file", "md"];
const VALID_ALSO_TOKENS: &[&str] = &["lance", "md", "json_cache"];
const VALID_LANCE_MODES: &[&str] = &["create", "append", "overwrite"];

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            authoritative: default_authoritative(),
            also: default_also_md(),
            json_cache: JsonCacheConfig::default(),
            lance: LanceStorageConfig::default(),
        }
    }
}

impl Default for LanceStorageConfig {
    fn default() -> Self {
        Self {
            db_uri: String::new(),
            table_name: default_lance_table_name(),
            backend: default_lance_backend(),
            mode: default_lance_mode(),
            fail_open: default_fail_open(),
            dead_letter_path: default_dead_letter_path(),
            s3: None,
            batch_size: default_batch_size(),
            flush_interval_ms: 0,
            write_timeout_ms: 0,
            async_writer: false,
        }
    }
}

fn default_lance_table_name() -> String {
    "session_steps".to_string()
}

fn default_lance_backend() -> String {
    "lance".to_string()
}

fn default_lance_mode() -> String {
    "append".to_string()
}

fn default_fail_open() -> bool {
    true
}

fn default_dead_letter_path() -> String {
    ".capture/lance_dead_letter.jsonl".to_string()
}

fn default_batch_size() -> u32 {
    1
}
