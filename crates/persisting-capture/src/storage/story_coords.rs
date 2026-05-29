//! Run / Story coordinates for offline trajectory egress (aligned with [`CaptureRoute`]).
//!
//! - **Run** → `root_session_id` → `{storage}/{agent_id}/{run_id}/`
//! - **Story** → `session_id` → Lance key + `{session_id}.md` under the run directory

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

    pub fn lance_dataset_dir(&self) -> Result<PathBuf> {
        story_lance_dataset_dir(
            &self.storage,
            &self.agent_id,
            &self.session_id,
            self.root_session_id.as_deref(),
        )
    }

    pub fn lance_uri_display(&self) -> Result<String> {
        Ok(self.lance_dataset_dir()?.to_string_lossy().into_owned())
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

/// Lance dataset directory (`.lance/{session_key}/` under run dir when nested).
pub fn story_lance_dataset_dir(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    let run = story_run_dir(storage, agent_id, session_id, root_session_id)?;
    match root_session_id {
        Some(_) => {
            let key = validate_path_segment(session_id, "session_id")?;
            Ok(run.join(".lance").join(key))
        }
        None => Ok(run),
    }
}
