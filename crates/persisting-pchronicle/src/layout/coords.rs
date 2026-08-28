//! Run / Story coordinates for offline trajectory storage.
//!
//! - **Run** → `root_session_id` → `{storage}/{agent_id}/{run_id}/`
//! - **Story** → `session_id` → `{session_id}.md` under the run directory; Lance rows are filtered by session_id

use std::path::{Path, PathBuf};

use anyhow::Result;

/// Offline story coordinates shared by pChronicle clients.
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

    pub fn run_dir(&self) -> Result<PathBuf> {
        story_run_dir(
            &self.storage,
            &self.agent_id,
            &self.session_id,
            self.root_session_id.as_deref(),
        )
    }

    pub fn lance_event_path(&self) -> Result<PathBuf> {
        story_lance_event_path(
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

/// Epoch-fenced Lance event log root at `{run}/events.lance/`.
pub fn story_lance_event_path(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    let run = story_run_dir(storage, agent_id, session_id, root_session_id)?;
    Ok(run.join("events.lance"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn segment_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9_-]{1,16}")
            .unwrap()
            .prop_map(|value| value)
    }

    fn padded_segment_strategy() -> impl Strategy<Value = (String, String)> {
        (segment_strategy(), 0usize..=3, 0usize..=3).prop_map(|(value, left, right)| {
            let original = format!("{}{}{}", " ".repeat(left), value, " ".repeat(right));
            (original, value)
        })
    }

    #[test]
    fn nested_sessions_share_run_level_lance_path() {
        let root = story_lance_event_path("/store", "agent", "run-001", Some("run-001")).unwrap();
        let sub = story_lance_event_path("/store", "agent", "agent-sub", Some("run-001")).unwrap();
        assert_eq!(root, sub);
        assert!(root.ends_with("agent/run-001/events.lance"));
    }

    #[test]
    fn flat_raw_event_lance_path_is_session_scoped() {
        let path = story_lance_event_path("/store", "agent", "sess-flat", None).unwrap();
        assert!(path.ends_with("agent/sess-flat/events.lance"));
    }

    #[test]
    fn story_coords_lance_event_path_matches_helper() {
        let coords = StoryCoords::new("/store", "agent", "run-x", Some("run-x".into()));
        assert_eq!(
            coords.lance_event_path().unwrap(),
            story_lance_event_path("/store", "agent", "run-x", Some("run-x")).unwrap()
        );
    }

    #[test]
    fn object_store_uri_preserves_scheme_and_run_partitioning() {
        let root = story_lance_event_path(
            "s3://trajectory-bucket/prefix",
            "agent",
            "run-001",
            Some("run-001"),
        )
        .unwrap();
        let child = story_lance_event_path(
            "s3://trajectory-bucket/prefix",
            "agent",
            "child-001",
            Some("run-001"),
        )
        .unwrap();
        assert_eq!(root, child);
        assert_eq!(
            root.to_string_lossy(),
            "s3://trajectory-bucket/prefix/agent/run-001/events.lance"
        );
    }

    proptest! {
        #[test]
        fn valid_segments_are_trimmed_but_remain_single_path_components(
            (agent, agent_trimmed) in padded_segment_strategy(),
            (session, session_trimmed) in padded_segment_strategy(),
        ) {
            let path = story_run_dir("/store", &agent, &session, None).unwrap();
            let suffix = format!("{}/{}", agent_trimmed, session_trimmed);
            prop_assert!(path.ends_with(suffix));
            prop_assert!(!agent_trimmed.contains('/'));
            prop_assert!(!session_trimmed.contains('/'));
        }

        #[test]
        fn nested_sessions_share_the_root_run_partition(
            agent in segment_strategy(),
            root in segment_strategy(),
            child in segment_strategy(),
        ) {
            let root_path = story_lance_event_path("/store", &agent, &root, Some(&root)).unwrap();
            let child_path = story_lance_event_path("/store", &agent, &child, Some(&root)).unwrap();
            prop_assert_eq!(root_path, child_path);
        }

        #[test]
        fn invalid_storage_and_segments_fail_closed(
            agent in segment_strategy(),
            session in segment_strategy(),
            invalid in prop_oneof![
                Just("".to_string()),
                Just("   ".to_string()),
                Just(".".to_string()),
                Just("..".to_string()),
                Just("a/b".to_string()),
                Just("a\\b".to_string()),
            ],
        ) {
            prop_assert!(story_run_dir("", &agent, &session, None).is_err());
            prop_assert!(story_run_dir("/store", &invalid, &session, None).is_err());
            prop_assert!(story_run_dir("/store", &agent, &invalid, None).is_err());
        }

        #[test]
        fn coords_methods_delegate_to_the_free_functions(
            storage in prop_oneof![Just("/store".to_string()), Just("s3://bucket/prefix".to_string())],
            agent in segment_strategy(),
            session in segment_strategy(),
            root in prop::option::of(segment_strategy()),
        ) {
            let coords = StoryCoords::new(storage.clone(), agent.clone(), session.clone(), root.clone());
            prop_assert_eq!(
                coords.run_dir().unwrap(),
                story_run_dir(&storage, &agent, &session, root.as_deref()).unwrap(),
            );
            prop_assert_eq!(
                coords.lance_event_path().unwrap(),
                story_lance_event_path(&storage, &agent, &session, root.as_deref()).unwrap(),
            );
        }
    }
}
