//! Environment variables for `capture run` child processes and daemon snapshots.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::ProxyConfig;

pub const ENV_SESSION_ID: &str = "PERSISTING_CAPTURE_SESSION_ID";
pub const DAEMON_ENV_FILENAME: &str = "daemon.env.json";

/// Env vars that route HTTP clients through the capture proxy (must not be set on the daemon).
pub const CAPTURE_PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// API keys snapshotted into `{storage}/.capture/daemon.env.json` at daemon start.
pub const STANDARD_DAEMON_ENV_KEYS: &[&str] = &[
    "DEEPSEEK_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonEnvSnapshot {
    pub snapshotted_at: String,
    pub vars: HashMap<String, String>,
}

pub fn daemon_env_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join(DAEMON_ENV_FILENAME)
}

pub fn run_session_file(storage: &Path) -> PathBuf {
    storage.join(".capture").join("run_session")
}

pub fn write_run_session(storage: &Path, session_id: &str) -> Result<()> {
    let path = run_session_file(storage);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, session_id.trim()).context("write run_session")?;
    Ok(())
}

pub fn read_run_session(storage: &Path) -> Option<String> {
    let path = run_session_file(storage);
    let s = fs::read_to_string(&path).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Default serve-mode run bucket: UTC calendar day (`YYYY-MM-DD`).
pub fn daily_run_id() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Ensure `.capture/run_session` exists for long-running serve (daily bucket unless preset).
pub fn ensure_serve_run_session(storage: &Path) -> Result<String> {
    if let Some(run) = read_run_session(storage) {
        return Ok(run);
    }
    let day = daily_run_id();
    write_run_session(storage, &day)?;
    Ok(day)
}

pub const RUN_CHILD_FILENAME: &str = "run_child.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunChildInfo {
    pub pid: u32,
    pub command: String,
}

pub fn run_child_file(storage: &Path) -> PathBuf {
    storage.join(".capture").join(RUN_CHILD_FILENAME)
}

/// Written at `capture run` child spawn — authoritative client command (not the proxy).
pub fn write_run_child_info(storage: &Path, pid: u32, command: &[String]) -> Result<()> {
    let path = run_child_file(storage);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let info = RunChildInfo {
        pid,
        command: command.join(" "),
    };
    let yaml = serde_yaml::to_string(&info).context("serialize run_child.yaml")?;
    fs::write(&path, yaml).context("write run_child.yaml")
}

pub(crate) fn read_run_child_info(storage: &Path) -> Option<RunChildInfo> {
    let path = run_child_file(storage);
    let text = fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&text).ok()
}

/// Keys to snapshot: standard API keys + `api_key_env` from yaml model routes.
pub fn collect_daemon_env_keys(config: &ProxyConfig) -> Vec<String> {
    let mut keys: Vec<String> = STANDARD_DAEMON_ENV_KEYS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for route in &config.models {
        if let Some(env) = &route.api_key_env {
            if !keys.iter().any(|k| k == env) {
                keys.push(env.clone());
            }
        }
    }
    keys
}

/// Snapshot whitelisted env vars from the current process at `capture start` / `capture run -c`.
pub fn snapshot_daemon_env(storage: &Path, config: &ProxyConfig) -> Result<PathBuf> {
    let mut vars = HashMap::new();
    for key in collect_daemon_env_keys(config) {
        if is_capture_proxy_env_key(&key) {
            continue;
        }
        if let Ok(v) = std::env::var(&key) {
            let v = v.trim();
            if !v.is_empty() {
                vars.insert(key, v.to_string());
            }
        }
    }
    let snap = DaemonEnvSnapshot {
        snapshotted_at: chrono::Utc::now().to_rfc3339(),
        vars: expand_daemon_env_aliases(vars, config),
    };
    let path = daemon_env_path(storage);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&snap)?).context("write daemon.env.json")?;
    Ok(path)
}

/// Fill route `api_key_env` entries from known alias vars (e.g. `ANTHROPIC_AUTH_TOKEN` → `DEEPSEEK_API_KEY`).
pub fn expand_daemon_env_aliases(
    mut vars: HashMap<String, String>,
    config: &ProxyConfig,
) -> HashMap<String, String> {
    for route in &config.models {
        let Some(primary) = &route.api_key_env else {
            continue;
        };
        if vars.contains_key(primary) {
            continue;
        }
        for alias in crate::config::api_key_env_aliases(primary) {
            if let Some(v) = vars.get(*alias) {
                vars.insert(primary.clone(), v.clone());
                break;
            }
        }
    }
    vars
}

pub fn load_daemon_env_snapshot(storage: &Path) -> Result<Option<DaemonEnvSnapshot>> {
    let path = daemon_env_path(storage);
    if !path.is_file() {
        return Ok(None);
    }
    let s = fs::read_to_string(&path).context("read daemon.env.json")?;
    Ok(Some(
        serde_json::from_str(&s).context("parse daemon.env.json")?,
    ))
}

pub fn strip_capture_proxy_env() {
    for key in CAPTURE_PROXY_ENV_KEYS {
        unsafe {
            std::env::remove_var(key);
        }
    }
}

/// Apply snapshotted daemon env on `capture serve` startup; always strips proxy env vars.
pub fn apply_daemon_env(storage: &Path) -> Result<Vec<String>> {
    strip_capture_proxy_env();
    let Some(snap) = load_daemon_env_snapshot(storage)? else {
        return Ok(vec![]);
    };
    let mut applied = Vec::new();
    for (k, v) in snap.vars {
        unsafe {
            std::env::set_var(&k, &v);
        }
        applied.push(k);
    }
    applied.sort();
    Ok(applied)
}

fn is_capture_proxy_env_key(key: &str) -> bool {
    CAPTURE_PROXY_ENV_KEYS
        .iter()
        .any(|p| p.eq_ignore_ascii_case(key))
}

/// OpenAI-compatible gateway base (`http://127.0.0.1:PORT/v1`) for child LLM clients.
pub fn capture_openai_v1_base(listen: &str) -> String {
    let base = if listen.starts_with("http://") || listen.starts_with("https://") {
        listen.to_string()
    } else {
        format!("http://{listen}")
    };
    format!("{}/v1", base.trim_end_matches('/'))
}

/// Extra CLI flags for clients that ignore `OPENAI_BASE_URL` and need explicit config overrides.
///
/// Codex reads `openai_base_url` from `config.toml` (via `-c`), not from `OPENAI_BASE_URL`.
pub fn client_gateway_config_args(program: &str, listen: &str) -> Vec<String> {
    let openai_v1 = capture_openai_v1_base(listen);
    let name = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    match name {
        "codex" => vec!["-c".to_string(), format!("openai_base_url=\"{openai_v1}\"")],
        _ => Vec::new(),
    }
}

/// Build env map for subprocess: HTTP(S) forward proxy + LLM SDK base URLs.
///
/// Child processes that honor `HTTP_PROXY` / `HTTPS_PROXY` send **all** HTTP(S) traffic
/// to `listen`. The capture server:
/// - `CONNECT` → TCP tunnel (HTTPS and other TLS)
/// - absolute-URI HTTP → transparent forward, except LLM API paths (captured + yaml upstream)
/// - relative paths on `listen` (via `OPENAI_BASE_URL`) → LLM gateway + capture
pub fn proxy_environment(listen: &str, session_id: &str) -> HashMap<String, String> {
    let base = if listen.starts_with("http://") || listen.starts_with("https://") {
        listen.to_string()
    } else {
        format!("http://{listen}")
    };
    let base = base.trim_end_matches('/').to_string();
    let openai_v1 = capture_openai_v1_base(listen);

    let mut env = HashMap::new();
    for key in CAPTURE_PROXY_ENV_KEYS {
        env.insert(key.to_string(), base.clone());
    }
    // Loopback gateway requests must not be CONNECT-tunneled via HTTPS_PROXY.
    env.insert("NO_PROXY".to_string(), "127.0.0.1,localhost".to_string());
    env.insert("no_proxy".to_string(), "127.0.0.1,localhost".to_string());
    for key in [
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "AZURE_OPENAI_ENDPOINT",
    ] {
        env.insert(key.to_string(), openai_v1.clone());
    }
    env.insert("ANTHROPIC_BASE_URL".to_string(), base.clone());
    env.insert("GEMINI_API_BASE".to_string(), format!("{base}/v1beta"));
    env.insert(ENV_SESSION_ID.to_string(), session_id.to_string());

    // Claude Code + DeepSeek Anthropic surface reads ANTHROPIC_AUTH_TOKEN.
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), key.clone());
        env.insert("ANTHROPIC_API_KEY".to_string(), key);
    }

    env
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ProxyConfig {
        ProxyConfig::from_yaml_str(
            r#"
listen: "127.0.0.1:1"
admin_listen: "127.0.0.1:2"
agent_id: "t"
models:
  - name: "*"
    upstream: "http://example.com"
    api_key_env: "DEEPSEEK_API_KEY"
"#,
        )
        .unwrap()
    }

    #[test]
    fn proxy_env_keys() {
        let env = proxy_environment("127.0.0.1:8080", "sess-1");
        assert_eq!(
            env.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            env.get("NO_PROXY").map(String::as_str),
            Some("127.0.0.1,localhost")
        );
        assert_eq!(env.get(ENV_SESSION_ID).map(String::as_str), Some("sess-1"));
    }

    #[test]
    fn codex_gateway_config_args() {
        let args = client_gateway_config_args("codex", "127.0.0.1:19081");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "openai_base_url=\"http://127.0.0.1:19081/v1\"");
    }

    #[test]
    fn env_alias_expansion() {
        let cfg = test_config();
        let vars = expand_daemon_env_aliases(
            HashMap::from([("ANTHROPIC_AUTH_TOKEN".into(), "sk-ds".into())]),
            &cfg,
        );
        assert_eq!(
            vars.get("DEEPSEEK_API_KEY").map(String::as_str),
            Some("sk-ds")
        );
    }

    #[test]
    fn daemon_env_apply_strips_proxy_and_sets_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path();
        let snap = DaemonEnvSnapshot {
            snapshotted_at: "2026-01-01T00:00:00Z".into(),
            vars: HashMap::from([("DEEPSEEK_API_KEY".into(), "sk-test".into())]),
        };
        fs::create_dir_all(storage.join(".capture")).unwrap();
        fs::write(
            daemon_env_path(storage),
            serde_json::to_string_pretty(&snap).unwrap(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("HTTP_PROXY", "http://127.0.0.1:19081");
        }
        let applied = apply_daemon_env(storage).unwrap();
        assert!(applied.contains(&"DEEPSEEK_API_KEY".to_string()));
        assert_eq!(std::env::var("DEEPSEEK_API_KEY").unwrap(), "sk-test");
        assert!(std::env::var("HTTP_PROXY").is_err());

        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
    }

    #[test]
    fn collect_keys_from_yaml() {
        let cfg = test_config();
        let keys = collect_daemon_env_keys(&cfg);
        assert!(keys.contains(&"DEEPSEEK_API_KEY".to_string()));
    }
}
