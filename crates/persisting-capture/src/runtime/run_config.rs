//! Per-`capture run` proxy config snapshot (`{storage}/.capture/sessions/{session_id}/proxy.toml`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::ProxyConfig;

pub const SESSION_PROXY_FILENAME: &str = "proxy.toml";
pub const LEGACY_SESSION_PROXY_FILENAME: &str = "proxy.yaml";

pub fn session_dir(storage: &Path, session_id: &str) -> PathBuf {
    storage
        .join(".capture")
        .join("sessions")
        .join(session_id.trim())
}

pub fn session_proxy_config_path(storage: &Path, session_id: &str) -> PathBuf {
    session_dir(storage, session_id).join(SESSION_PROXY_FILENAME)
}

/// Parse source config and write canonical TOML into the session snapshot (used by `capture run`).
pub fn snapshot_run_proxy_config(
    storage: &Path,
    session_id: &str,
    source: &Path,
) -> Result<PathBuf> {
    let cfg = ProxyConfig::from_file(source)?;
    let dest = session_proxy_config_path(storage, session_id);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&dest, cfg.to_toml_string()?).with_context(|| {
        format!(
            "write proxy config snapshot {} (from {})",
            dest.display(),
            source.display()
        )
    })?;
    Ok(dest)
}

/// Load snapshotted config for a session; `None` if no snapshot (fallback to daemon default).
pub fn load_session_proxy_config(storage: &Path, session_id: &str) -> Option<ProxyConfig> {
    let dir = session_dir(storage, session_id);
    for name in [SESSION_PROXY_FILENAME, LEGACY_SESSION_PROXY_FILENAME] {
        let path = dir.join(name);
        if path.is_file() {
            if let Ok(cfg) = ProxyConfig::from_file(&path) {
                return Some(cfg);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn snapshot_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path();
        let src = storage.join("src.toml");
        std::fs::File::create(&src)
            .unwrap()
            .write_all(
                br#"listen = "127.0.0.1:19999"
admin_listen = "127.0.0.1:19998"
agent_id = "snap-agent"

[[models]]
name = "*"
upstream = "http://127.0.0.1:1"
"#,
            )
            .unwrap();
        let dest = snapshot_run_proxy_config(storage, "sess-a", &src).unwrap();
        assert!(dest.ends_with("proxy.toml"));
        let cfg = load_session_proxy_config(storage, "sess-a").unwrap();
        assert_eq!(cfg.agent_id, "snap-agent");
        assert_eq!(cfg.listen, "127.0.0.1:19999");
    }
}
