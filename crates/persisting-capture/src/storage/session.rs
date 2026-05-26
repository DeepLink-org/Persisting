//! Session routing and run-directory storage (`{run}/{session_id}.md`).

use std::path::{Path, PathBuf};

use axum::http::HeaderMap;
use bytes::Bytes;

use super::dialogue_extract::{is_main_agent_continuation, is_subagent_shape_payload};
use super::markdown::{is_subagent_session_storage_key, session_markdown_write_path_for_key};
use crate::run_env::read_run_session;
use crate::session_chain::{
    ephemeral_session_id, extract_claude_agent_id, extract_session_from_headers,
};

/// Where to append captures for one proxied LLM request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CaptureRoute {
    /// Run directory name under `{storage}/{agent_id}/` (capture run or daily serve bucket).
    pub root_session: Option<String>,
    /// Logical session from request headers (stored on [`CaptureRecord`]).
    pub session_id: String,
    /// Markdown filename stem and Lance key (`{storage_session_id}.md`).
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

    /// Run id for nested append (always set when [`Self::root_session`] is present).
    pub fn append_root_session(&self) -> Option<String> {
        self.root_session.clone()
    }

    /// Route for offline replay / reconcile (main session omits `root_session` wrapper).
    pub fn for_replay_stem(root_session: &str, session_id: &str) -> Self {
        let subagent_id = is_subagent_session_storage_key(session_id)
            .then(|| session_id.strip_prefix("agent-").unwrap_or(session_id).to_string());
        Self {
            root_session: if session_id == root_session {
                None
            } else {
                Some(root_session.to_string())
            },
            session_id: session_id.to_string(),
            storage_session_id: session_id.to_string(),
            subagent_id,
        }
    }

    /// Route for live run markdown frontmatter refresh (`root_session` always set).
    pub fn for_run_markdown_stem(root_session: &str, stem: &str) -> Self {
        let subagent_id = is_subagent_session_storage_key(stem)
            .then(|| stem.strip_prefix("agent-").unwrap_or(stem).to_string());
        Self {
            root_session: Some(root_session.to_string()),
            session_id: stem.to_string(),
            storage_session_id: stem.to_string(),
            subagent_id,
        }
    }
}

fn subagent_storage_leaf(claude_agent_id: &str) -> String {
    format!("agent-{claude_agent_id}")
}

fn main_storage_session_id(header_session: &Option<String>, run_id: &str) -> String {
    header_session.clone().unwrap_or_else(|| run_id.to_string())
}

/// Resolve routing: header session wins for filename; subagents use `agent-{id}.md` siblings.
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
        None => {
            let storage_session_id = if let Some(agent_id) = claude_agent_id.as_deref() {
                subagent_storage_leaf(agent_id)
            } else {
                session_id.clone()
            };
            CaptureRoute {
                root_session: None,
                session_id,
                storage_session_id,
                subagent_id: claude_agent_id,
            }
        }
        Some(root) => {
            if claude_agent_id.is_some() && is_main_agent_continuation(body) {
                CaptureRoute {
                    root_session: Some(root.clone()),
                    session_id,
                    storage_session_id: main_storage_session_id(&header_session, &root),
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
                    storage_session_id: main_storage_session_id(&header_session, &root),
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

/// Run directory under `{storage}/{agent_id}/…` (all sessions in one capture run share this dir).
pub fn trajectory_run_dir(storage: &Path, agent_id: &str, route: &CaptureRoute) -> PathBuf {
    let base = storage.join(agent_id);
    match &route.root_session {
        Some(root) => base.join(root),
        None => base.join(&route.storage_session_id),
    }
}

/// Alias for run directory (legacy name used by session client metadata).
pub fn trajectory_session_dir(storage: &Path, agent_id: &str, route: &CaptureRoute) -> PathBuf {
    trajectory_run_dir(storage, agent_id, route)
}

/// Full path to the session markdown file (`{run_dir}/{storage_session_id}.md`).
pub fn trajectory_markdown_path(storage: &Path, agent_id: &str, route: &CaptureRoute) -> PathBuf {
    let run_dir = trajectory_run_dir(storage, agent_id, route);
    session_markdown_write_path_for_key(&run_dir, &route.storage_session_id)
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
    fn main_agent_writes_header_session_md_in_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-chat","messages":[{"role":"user","content":"hi"}]}"#,
        );
        let header_session = "e96634a3-fa28-4083-b354-55542e2dca01";
        let h = headers(&[("x-claude-code-session-id", header_session)]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.root_session.as_deref(), Some("my-run"));
        assert_eq!(route.storage_session_id, header_session);
        assert_eq!(route.session_id, header_session);
        assert!(route.subagent_id.is_none());
        let run = trajectory_run_dir(dir.path(), "agent", &route);
        assert_eq!(run, dir.path().join("agent/my-run"));
        let md = trajectory_markdown_path(dir.path(), "agent", &route);
        assert_eq!(md, run.join(format!("{header_session}.md")));
    }

    #[test]
    fn flash_probe_without_agent_id_writes_header_session_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":[{"type":"text","text":"<session>\nexplore repo with three subagents\n</session>"}]}]}"#,
        );
        let header_session = "884e189a-9433-4996-a809-3a8232a8967d";
        let h = headers(&[("x-claude-code-session-id", header_session)]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.storage_session_id, header_session);
        let md = trajectory_markdown_path(dir.path(), "agent", &route);
        assert!(md.ends_with(format!("{header_session}.md")));
    }

    #[test]
    fn haiku_session_wrapper_without_agent_id_writes_header_session_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"claude-haiku-4-5","messages":[{"role":"user","content":[{"type":"text","text":"<session>\ntask\n</session>"}]}]}"#,
        );
        let header_session = "sub-session-uuid-12345678";
        let h = headers(&[("x-claude-code-session-id", header_session)]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.storage_session_id, header_session);
        let md = trajectory_markdown_path(dir.path(), "agent", &route);
        assert!(md.ends_with(format!("{header_session}.md")));
    }

    #[test]
    fn parallel_subagents_use_sibling_agent_md_files() {
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
            let run = trajectory_run_dir(dir.path(), "agent", &route);
            assert_eq!(run, dir.path().join("agent/my-run"));
            let md = trajectory_markdown_path(dir.path(), "agent", &route);
            assert_eq!(md, run.join(format!("agent-{agent}.md")));
        }
    }

    #[test]
    fn serve_flat_layout() {
        let body = Bytes::from_static(b"{}");
        let h = headers(&[("x-persisting-session-id", "sess-flat")]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", Path::new("/tmp"));
        assert!(route.root_session.is_none());
        assert_eq!(route.storage_session_id, "sess-flat");
        let md = trajectory_markdown_path(Path::new("/tmp"), "agent", &route);
        assert!(md.ends_with("sess-flat/sess-flat.md"));
    }

    #[test]
    fn no_header_falls_back_to_run_id_md() {
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
        let md = trajectory_markdown_path(dir.path(), "agent", &route);
        assert!(md.ends_with("run-only/run-only.md"));
    }

    #[test]
    fn task_notification_with_agent_id_writes_main_session_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".capture")).unwrap();
        std::fs::write(dir.path().join(".capture/run_session"), "my-run").unwrap();
        let body = Bytes::from_static(
            br#"{"model":"deepseek-v4-pro","messages":[{"role":"user","content":[{"type":"text","text":"<task-notification>\n<task-id>aeb40a1230926b4a8</task-id>\n<status>completed</status>\n</task-notification>"}]}]}"#,
        );
        let header_session = "c9df8c43-e129-47d3-93c3-7cedfab86931";
        let h = headers(&[
            ("x-claude-code-session-id", header_session),
            ("x-claude-code-agent-id", "aeb40a1230926b4a8"),
        ]);
        let route = resolve_capture_route(&h, &body, "x-persisting-session-id", dir.path());
        assert_eq!(route.storage_session_id, header_session);
        assert!(route.subagent_id.is_none());
        let md = trajectory_markdown_path(dir.path(), "agent", &route);
        assert!(md.ends_with(format!("{header_session}.md")));
    }
}
