//! Per-`capture run` proxy config snapshot (`{storage}/.capture/sessions/{session_id}/proxy.toml`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::ProxyConfig;
use crate::runtime::private_fs::{ensure_private_dir, write_private_file};
use crate::runtime::run_env::ensure_private_capture_dir;

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
    snapshot_proxy_config(storage, session_id, &cfg)
}

/// Write an already-resolved proxy configuration into the session snapshot.
/// pVisor uses this after merging its TOML and CLI configuration, so Gateway
/// runtime setup never needs to know where configuration originated.
pub fn snapshot_proxy_config(
    storage: &Path,
    session_id: &str,
    cfg: &ProxyConfig,
) -> Result<PathBuf> {
    let dest = session_proxy_config_path(storage, session_id);
    let capture_dir = ensure_private_capture_dir(storage)?;
    let sessions_dir = capture_dir.join("sessions");
    ensure_private_dir(&sessions_dir)?;
    ensure_private_dir(&session_dir(storage, session_id))?;
    let toml = cfg.to_toml_string()?;
    write_private_file(&dest, toml.as_bytes())
        .with_context(|| format!("write proxy config snapshot {}", dest.display()))?;
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

    #[cfg(unix)]
    #[test]
    fn snapshot_is_private_even_when_replacing_a_public_file() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path();
        let cfg = ProxyConfig::from_toml_str(
            r#"
listen = "127.0.0.1:19999"
admin_listen = "127.0.0.1:19998"
agent_id = "private-snapshot"

[[models]]
name = "*"
upstream = "http://127.0.0.1:1"
api_key = "sk-inline-secret"
"#,
        )
        .unwrap();
        let dest = session_proxy_config_path(storage, "sess-private");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "legacy public content").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).unwrap();

        snapshot_proxy_config(storage, "sess-private", &cfg).unwrap();

        for dir in [
            storage.join(".capture"),
            storage.join(".capture/sessions"),
            session_dir(storage, "sess-private"),
        ] {
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            load_session_proxy_config(storage, "sess-private")
                .unwrap()
                .models[0]
                .api_key
                .as_deref(),
            Some("sk-inline-secret")
        );
    }
}
