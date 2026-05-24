//! Per-`capture run` proxy config snapshot (`{storage}/.capture/sessions/{session_id}/proxy.yaml`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::ProxyConfig;

pub const SESSION_PROXY_FILENAME: &str = "proxy.yaml";

pub fn session_dir(storage: &Path, session_id: &str) -> PathBuf {
    storage
        .join(".capture")
        .join("sessions")
        .join(session_id.trim())
}

pub fn session_proxy_config_path(storage: &Path, session_id: &str) -> PathBuf {
    session_dir(storage, session_id).join(SESSION_PROXY_FILENAME)
}

/// Copy `source` yaml into the session snapshot path (used by `capture run`).
pub fn snapshot_run_proxy_config(
    storage: &Path,
    session_id: &str,
    source: &Path,
) -> Result<PathBuf> {
    let dest = session_proxy_config_path(storage, session_id);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, &dest).with_context(|| {
        format!(
            "snapshot proxy config {} -> {}",
            source.display(),
            dest.display()
        )
    })?;
    Ok(dest)
}

/// Load snapshotted config for a session; `None` if no snapshot (fallback to daemon default).
pub fn load_session_proxy_config(storage: &Path, session_id: &str) -> Option<ProxyConfig> {
    let path = session_proxy_config_path(storage, session_id);
    ProxyConfig::from_yaml_file(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn snapshot_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path();
        let src = storage.join("src.yaml");
        std::fs::File::create(&src)
            .unwrap()
            .write_all(
                br#"listen: "127.0.0.1:19999"
admin_listen: "127.0.0.1:19998"
agent_id: "snap-agent"
models:
  - name: "*"
    upstream: "http://127.0.0.1:1"
"#,
            )
            .unwrap();
        let dest = snapshot_run_proxy_config(storage, "sess-a", &src).unwrap();
        assert!(dest.is_file());
        let cfg = load_session_proxy_config(storage, "sess-a").unwrap();
        assert_eq!(cfg.agent_id, "snap-agent");
        assert_eq!(cfg.listen, "127.0.0.1:19999");
    }
}
