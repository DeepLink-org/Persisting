//! Session routing and nested storage paths (`{root}/subagents/{session}/`).

use std::path::{Path, PathBuf};

use axum::http::HeaderMap;
use bytes::Bytes;

use crate::dialogue_extract::{is_main_agent_continuation, is_subagent_shape_payload};
use crate::run_env::read_run_session;
use crate::session_chain::{
    ephemeral_session_id, extract_claude_agent_id, extract_session_from_headers,
};

/// Where to append captures for one proxied LLM request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRoute {
    /// Root session directory from `capture run` (`.capture/run_session`); the storage path is the job.
    pub root_session: Option<String>,
    /// Logical session from request headers (stored on [`CaptureRecord`]).
    pub session_id: String,
    /// Engine path segment passed as `session_id` (root or subagent leaf).
    pub storage_session_id: String,
    /// Claude Code subagent instance id (`x-claude-code-agent-id`), when present.
    pub subagent_id: Option<String>,
}

impl CaptureRoute {
    pub fn seq_key(&self) -> String {
        match &self.root_session {
            Some(root) => format!("{root}|{}", self.storage_session_id),
            None => self.storage_session_id.clone(),
        }
    }

    /// Nested append: subagent under `{root}/subagents/{leaf}/`.
    pub fn append_root_session(&self) -> Option<String> {
        self.root_session
            .as_ref()
            .filter(|root| self.storage_session_id.as_str() != root.as_str())
            .cloned()
    }
}

fn subagent_storage_leaf(claude_agent_id: &str) -> String {
    format!("agent-{claude_agent_id}")
}

/// Resolve routing: header session wins; fallback run root; subagents nest under `{root}/subagents/`.
pub fn resolve_capture_route(
    headers: &HeaderMap,
    body: &Bytes,
    session_header: &str,
    storage: &Path,
) -> CaptureRoute {
    let root_session = read_run_session(storage);
    let header_session = extract_session_from_headers(headers, session_header);
    let claude_agent_id = extract_claude_agent_id(headers);
    let _body_subagent = is_subagent_request_body(body);

    let session_id = header_session
        .clone()
        .unwrap_or_else(|| root_session.clone().unwrap_or_else(ephemeral_session_id));

    match root_session {
        None => CaptureRoute {
            root_session: None,
            session_id: session_id.clone(),
            storage_session_id: session_id,
            subagent_id: claude_agent_id,
        },
        Some(root) => {
            // Nested subagent dirs require `x-claude-code-agent-id`. Flash/haiku probes
            // with `<session>` wrapper but no agent id stay on the main run directory.
            // Subagent completion notifications also stay on main even when the header
            // still carries the completing agent's id.
            if claude_agent_id.is_some() && is_main_agent_continuation(body) {
                CaptureRoute {
                    root_session: Some(root.clone()),
                    session_id,
                    storage_session_id: root,
                    subagent_id: None,
                }
            } else if let Some(agent_id) = claude_agent_id {
                CaptureRoute {
                    root_session: Some(root),
                    session_id,
                    storage_session_id: subagent_storage_leaf(&agent_id),
                    subagent_id: Some(agent_id),
                }
            } else {
                CaptureRoute {
                    root_session: Some(root.clone()),
                    session_id,
                    storage_session_id: root,
                    subagent_id: None,
                }
            }
        }
    }
}

fn is_subagent_request_body(body: &Bytes) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .is_some_and(|v| is_subagent_shape_payload(&v))
}

/// Physical session directory under `{storage}/{agent_id}/…`.
pub fn trajectory_session_dir(storage: &Path, agent_id: &str, route: &CaptureRoute) -> PathBuf {
    let base = storage.join(agent_id);
    match (&route.root_session, route.storage_session_id.as_str()) {
        (Some(root), sid) if sid == root.as_str() => base.join(root),
        (Some(root), sid) => base.join(root).join("subagents").join(sid),
        (None, sid) => base.join(sid),
    }
}

/// Config / metadata key (run root when present).
pub fn route_config_key(route: &CaptureRoute) -> &str {
    route
        .root_session
        .as_deref()
        .unwrap_or(route.storage_session_id.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::HeaderName;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::try_from(*v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn main_agent_writes_root_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-chat","messages":[{"role":"user","content":"hi"}]}"#,
        );
        let h = headers(&[(
            "x-claude-code-session-id",
            "e96634a3-fa28-4083-b354-55542e2dca01",
        )]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.root_session.as_deref(), Some("my-run"));
        assert_eq!(route.storage_session_id, "my-run");
        assert_eq!(route.session_id, "e96634a3-fa28-4083-b354-55542e2dca01");
        assert!(route.subagent_id.is_none());
        let path = trajectory_session_dir(dir.path(), "agent", &route);
        assert_eq!(path, dir.path().join("agent/my-run"));
    }

    #[test]
    fn flash_probe_without_agent_id_writes_main_not_uuid_subdir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":[{"type":"text","text":"<session>\nexplore repo with three subagents\n</session>"}]}]}"#,
        );
        let h = headers(&[(
            "x-claude-code-session-id",
            "884e189a-9433-4996-a809-3a8232a8967d",
        )]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.root_session.as_deref(), Some("my-run"));
        assert_eq!(route.storage_session_id, "my-run");
        assert_eq!(route.session_id, "884e189a-9433-4996-a809-3a8232a8967d");
        assert!(route.subagent_id.is_none());
        let path = trajectory_session_dir(dir.path(), "agent", &route);
        assert_eq!(path, dir.path().join("agent/my-run"));
    }

    #[test]
    fn haiku_session_wrapper_without_agent_id_writes_main() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"claude-haiku-4-5","messages":[{"role":"user","content":[{"type":"text","text":"<session>\ntask\n</session>"}]}]}"#,
        );
        let h = headers(&[("x-claude-code-session-id", "sub-session-uuid-12345678")]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.storage_session_id, "my-run");
        assert!(route.subagent_id.is_none());
        let path = trajectory_session_dir(dir.path(), "agent", &route);
        assert_eq!(path, dir.path().join("agent/my-run"));
    }

    #[test]
    fn parallel_subagents_use_agent_id_leaf() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"explore auth"}]}"#,
        );
        let shared_session = "e96634a3-fa28-4083-b354-55542e2dca01";
        for agent in ["a111111", "b222222", "c333333"] {
            let h = headers(&[
                ("x-claude-code-session-id", shared_session),
                ("x-claude-code-agent-id", agent),
            ]);
            let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
            assert_eq!(route.subagent_id.as_deref(), Some(agent));
            assert_eq!(route.storage_session_id, format!("agent-{agent}"));
            let path = trajectory_session_dir(dir.path(), "agent", &route);
            assert_eq!(
                path,
                dir.path()
                    .join(format!("agent/my-run/subagents/agent-{agent}"))
            );
        }
    }

    #[test]
    fn serve_flat_layout() {
        let body = Bytes::from_static(b"{}");
        let h = headers(&[("x-persisting-session-id", "sess-flat")]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", Path::new("/tmp"));
        assert!(route.root_session.is_none());
        assert_eq!(route.storage_session_id, "sess-flat");
    }

    #[test]
    fn no_header_falls_back_to_run_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "run-only").unwrap();
        let body = Bytes::from_static(b"{}");
        let route = resolve_capture_route(
            &HeaderMap::new(),
            &body,
            "x-persisting-session-id",
            dir.path(),
        );
        assert_eq!(route.session_id, "run-only");
        assert_eq!(route.storage_session_id, "run-only");
    }

    #[test]
    fn task_notification_with_agent_id_writes_main() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-pro","messages":[{"role":"user","content":[{"type":"text","text":"<task-notification>\n<task-id>aeb40a1230926b4a8</task-id>\n<status>completed</status>\n</task-notification>"}]}]}"#,
        );
        let h = headers(&[
            (
                "x-claude-code-session-id",
                "c9df8c43-e129-47d3-93c3-7cedfab86931",
            ),
            ("x-claude-code-agent-id", "aeb40a1230926b4a8"),
        ]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.root_session.as_deref(), Some("my-run"));
        assert_eq!(route.storage_session_id, "my-run");
        assert!(route.subagent_id.is_none());
        let path = trajectory_session_dir(dir.path(), "agent", &route);
        assert_eq!(path, dir.path().join("agent/my-run"));
    }
}
