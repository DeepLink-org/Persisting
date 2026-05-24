//! Session client detection: peer port → process command line, persisted for trajectory headers.

use std::collections::HashSet;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::peer_process::resolve_peer_client;
use crate::run_env::{read_run_child_info, read_run_session};
use crate::session_storage::{trajectory_session_dir, CaptureRoute};

/// Sidecar metadata for `capture serve` (no `run_session`). `capture run` uses `run_child.yaml` + markdown frontmatter instead.
pub const SESSION_CLIENT_META_FILENAME: &str = "session-meta.yaml";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionClientMeta {
    pub peer: String,
    pub peer_port: u16,
    pub pid: u32,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionMetaFile {
    client: SessionClientMeta,
}

pub fn session_client_meta_path(storage: &Path, agent_id: &str, session_id: &str) -> PathBuf {
    storage
        .join(agent_id)
        .join(session_id)
        .join(SESSION_CLIENT_META_FILENAME)
}

pub fn session_client_meta_path_in_dir(session_dir: &Path) -> PathBuf {
    session_dir.join(SESSION_CLIENT_META_FILENAME)
}

pub fn load_session_client_meta(path: &Path) -> Result<Option<SessionClientMeta>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let file: SessionMetaFile =
        serde_yaml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(file.client))
}

pub fn write_session_client_meta(path: &Path, meta: &SessionClientMeta) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = SessionMetaFile {
        client: meta.clone(),
    };
    let yaml = serde_yaml::to_string(&file).context("serialize session-meta.yaml")?;
    fs::write(path, yaml).with_context(|| format!("write {}", path.display()))
}

/// Client metadata for markdown frontmatter: sidecar file, else `capture run` child snapshot.
pub fn resolve_client_meta_for_session_dir(
    storage: &Path,
    session_dir: &Path,
) -> Option<SessionClientMeta> {
    if let Ok(Some(m)) = load_session_client_meta(&session_client_meta_path_in_dir(session_dir)) {
        return Some(m);
    }
    let session_id = session_dir.file_name()?.to_str()?;
    read_run_session(storage)
        .as_deref()
        .filter(|j| *j == session_id)?;
    client_meta_from_run_child(
        storage,
        &CaptureRoute {
            root_session: Some(session_id.to_string()),
            session_id: session_id.to_string(),
            storage_session_id: session_id.to_string(),
            subagent_id: None,
        },
    )
}

fn is_capture_run_root(storage: &Path, route: &CaptureRoute) -> bool {
    read_run_session(storage).as_deref() == route.root_session.as_deref()
        && route
            .root_session
            .as_deref()
            .is_some_and(|r| r == route.storage_session_id.as_str())
}

fn client_meta_from_run_child(storage: &Path, route: &CaptureRoute) -> Option<SessionClientMeta> {
    if !is_capture_run_root(storage, route) {
        return None;
    }
    let run_child = read_run_child_info(storage)?;
    Some(SessionClientMeta {
        peer: String::new(),
        peer_port: 0,
        pid: run_child.pid,
        command: run_child.command,
    })
}

fn should_persist_session_meta_file(storage: &Path, route: &CaptureRoute) -> bool {
    !is_capture_run_root(storage, route)
}

/// Records client metadata once per session (best-effort; ignores lookup failures).
#[derive(Default)]
pub struct SessionClientRegistry {
    recorded: Mutex<HashSet<String>>,
}

impl SessionClientRegistry {
    pub fn ensure(
        &self,
        storage: &Path,
        agent_id: &str,
        route: &CaptureRoute,
        peer: SocketAddr,
    ) -> Option<SessionClientMeta> {
        let session_dir = trajectory_session_dir(storage, agent_id, route);
        let path = session_client_meta_path_in_dir(&session_dir);
        if should_persist_session_meta_file(storage, route) {
            if let Ok(Some(existing)) = load_session_client_meta(&path) {
                return Some(existing);
            }
        } else if let Some(m) = client_meta_from_run_child(storage, route) {
            return Some(SessionClientMeta {
                peer: peer.to_string(),
                peer_port: peer.port(),
                ..m
            });
        }

        let key = format!("{}|{agent_id}|{}", storage.display(), route.seq_key());
        {
            let guard = self.recorded.lock().unwrap();
            if guard.contains(&key) {
                if should_persist_session_meta_file(storage, route) {
                    return load_session_client_meta(&path).ok().flatten();
                }
                return client_meta_from_run_child(storage, route).map(|m| SessionClientMeta {
                    peer: peer.to_string(),
                    peer_port: peer.port(),
                    ..m
                });
            }
        }

        let mut meta = if is_capture_run_root(storage, route) {
            read_run_child_info(storage).map(|run_child| SessionClientMeta {
                peer: peer.to_string(),
                peer_port: peer.port(),
                pid: run_child.pid,
                command: run_child.command,
            })
        } else {
            None
        };

        if meta.is_none() {
            meta = match resolve_peer_client(peer) {
                Ok(Some(m)) => Some(m),
                Ok(None) => {
                    tracing::debug!(
                        target: "persisting_capture",
                        %peer,
                        session_id = %route.session_id,
                        "session client: no process found for peer port"
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(
                        target: "persisting_capture",
                        %peer,
                        session_id = %route.session_id,
                        "session client lookup failed: {e:#}"
                    );
                    None
                }
            };
        }

        let Some(meta) = meta else {
            self.recorded.lock().unwrap().insert(key);
            return None;
        };

        if should_persist_session_meta_file(storage, route) {
            if write_session_client_meta(&path, &meta).is_err() {
                return None;
            }
        }

        self.recorded.lock().unwrap().insert(key);
        tracing::debug!(
            target: "persisting_capture",
            peer = %meta.peer,
            pid = meta.pid,
            session_id = %route.session_id,
            command = %meta.command,
            "session client recorded"
        );
        Some(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_meta_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-meta.yaml");
        let meta = SessionClientMeta {
            peer: "127.0.0.1:54321".into(),
            peer_port: 54321,
            pid: 4242,
            command: "python3 agent.py --verbose".into(),
        };
        write_session_client_meta(&path, &meta).unwrap();
        let loaded = load_session_client_meta(&path).unwrap().unwrap();
        assert_eq!(loaded, meta);
    }

    #[test]
    fn ensure_prefers_run_child_over_lsof() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        std::fs::create_dir_all(storage.join(".capture")).unwrap();
        std::fs::write(storage.join(".capture/run_session"), "sess-a").unwrap();
        crate::run_env::write_run_child_info(storage, 4242, &["claude".into(), "--model".into()])
            .unwrap();

        let registry = SessionClientRegistry::default();
        let peer: std::net::SocketAddr = "127.0.0.1:55522".parse().unwrap();
        let route = CaptureRoute {
            root_session: Some("sess-a".into()),
            session_id: "sess-a".into(),
            storage_session_id: "sess-a".into(),
            subagent_id: None,
        };
        let meta = registry
            .ensure(storage, "agent", &route, peer)
            .expect("run_child metadata");
        assert_eq!(meta.command, "claude --model");
        assert_eq!(meta.pid, 4242);
        assert_eq!(meta.peer_port, 55522);
        assert!(
            !session_client_meta_path_in_dir(&storage.join("agent/sess-a")).exists(),
            "capture run should not write session-meta.yaml"
        );
    }

    #[test]
    fn resolve_client_meta_from_run_child() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path();
        std::fs::create_dir_all(storage.join(".capture")).unwrap();
        std::fs::write(storage.join(".capture/run_session"), "sess-b").unwrap();
        crate::run_env::write_run_child_info(storage, 99, &["python3".into(), "agent.py".into()])
            .unwrap();
        let session_dir = storage.join("agent").join("sess-b");
        std::fs::create_dir_all(&session_dir).unwrap();
        let meta = resolve_client_meta_for_session_dir(storage, &session_dir).unwrap();
        assert_eq!(meta.command, "python3 agent.py");
        assert_eq!(meta.pid, 99);
    }
}
