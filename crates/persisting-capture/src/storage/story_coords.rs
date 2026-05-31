//! Run / Story coordinates for offline trajectory egress (aligned with [`CaptureRoute`]).
//!
//! - **Run** → `root_session_id` → `{storage}/{agent_id}/{run_id}/`
//! - **Story** → `session_id` → `{session_id}.md` under the run directory; Vortex rows are filtered by session_id

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::session::CaptureRoute;

/// Offline story coordinates (same fields as engine/CLI `TrajLocation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryCoords {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub root_session_id: Option<String>,
}

impl StoryCoords {
    pub fn new(
        storage: impl Into<String>,
        agent_id: impl Into<String>,
        session_id: impl Into<String>,
        root_session_id: Option<String>,
    ) -> Self {
        Self {
            storage: storage.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            root_session_id,
        }
    }

    pub fn to_capture_route(&self) -> CaptureRoute {
        CaptureRoute::for_replay_stem(
            self.root_session_id
                .as_deref()
                .unwrap_or(self.session_id.as_str()),
            &self.session_id,
        )
    }

    pub fn run_dir(&self) -> Result<PathBuf> {
        story_run_dir(
            &self.storage,
            &self.agent_id,
            &self.session_id,
            self.root_session_id.as_deref(),
        )
    }

    pub fn vortex_event_path(&self) -> Result<PathBuf> {
        story_vortex_event_path(
            &self.storage,
            &self.agent_id,
            &self.session_id,
            self.root_session_id.as_deref(),
        )
    }
}

fn validate_storage(storage: &str) -> Result<()> {
    if storage.trim().is_empty() {
        anyhow::bail!("storage path must not be empty");
    }
    Ok(())
}

fn validate_path_segment(s: &str, field: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        anyhow::bail!("{field} must not be empty");
    }
    if t.contains('/') || t.contains('\\') {
        anyhow::bail!("{field} must not contain '/' or '\\' (single path segment only)");
    }
    if t == "." || t == ".." {
        anyhow::bail!("{field} must not be '.' or '..'");
    }
    Ok(t.to_string())
}

/// Run directory under `{storage}/{agent_id}/`.
pub fn story_run_dir(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    validate_storage(storage)?;
    let a = validate_path_segment(agent_id, "agent_id")?;
    match root_session_id {
        Some(root) => {
            let r = validate_path_segment(root, "root_session_id")?;
            Ok(Path::new(storage).join(a).join(r))
        }
        None => {
            let s = validate_path_segment(session_id, "session_id")?;
            Ok(Path::new(storage).join(a).join(s))
        }
    }
}

/// Vortex event log file at `{run}/events.vortex`.
pub fn story_vortex_event_path(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    let run = story_run_dir(storage, agent_id, session_id, root_session_id)?;
    Ok(run.join("events.vortex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_sessions_share_run_level_vortex_path() {
        let root = story_vortex_event_path("/store", "agent", "run-001", Some("run-001")).unwrap();
        let sub = story_vortex_event_path("/store", "agent", "agent-sub", Some("run-001")).unwrap();
        assert_eq!(root, sub);
        assert!(root.ends_with("agent/run-001/events.vortex"));
    }

    #[test]
    fn flat_session_vortex_path_is_session_scoped() {
        let path = story_vortex_event_path("/store", "agent", "sess-flat", None).unwrap();
        assert!(path.ends_with("agent/sess-flat/events.vortex"));
    }

    #[test]
    fn story_coords_vortex_event_path_matches_helper() {
        let coords = StoryCoords::new("/store", "agent", "run-x", Some("run-x".into()));
        assert_eq!(
            coords.vortex_event_path().unwrap(),
            story_vortex_event_path("/store", "agent", "run-x", Some("run-x")).unwrap()
        );
    }
}
