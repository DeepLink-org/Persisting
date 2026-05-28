//! Read-path: rebuild [`Story`] from persisted records and extract dialogue projections.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::story::{RunId, Story, StoryId, TurnKind, TurnMachine};
use crate::record::CaptureRecord;
use crate::session_storage::CaptureRoute;

/// On-disk cache of live story snapshots (keyed by markdown session stem).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct StorySnapshotsFile {
    pub updated_at: String,
    pub stories: HashMap<String, Story>,
    /// Denormalized for storage/frontmatter without importing [`Story`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub turn_counts: HashMap<String, u64>,
}

/// Rebuild in-memory story state by replaying persisted records (offline egress).
fn rebuild_story_from_records(
    story_id: StoryId,
    agent_id: impl Into<String>,
    run_id: Option<RunId>,
    records: &[CaptureRecord],
) -> Story {
    TurnMachine::replay_records(story_id, agent_id, run_id, records).snapshot()
}

/// Rebuild story for one session markdown stem from Lance records.
pub fn rebuild_session_story(
    session_id: &str,
    root_session: &str,
    records: &[CaptureRecord],
) -> Story {
    let route = CaptureRoute::for_replay_stem(root_session, session_id);
    let agent_id = records
        .iter()
        .find_map(|r| r.agent_id.clone())
        .unwrap_or_else(|| "unknown".into());
    let story_id = StoryId::from_seq_key(route.seq_key());
    let run_id = route.root_session.clone().map(RunId);
    rebuild_story_from_records(story_id, agent_id, run_id, records)
}

/// Visible call ids from the story read model.
pub fn story_call_ids(story: &Story) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for turn in &story.turns {
        if let Some(user) = &turn.user {
            if let Some(call_id) = &user.call_id {
                ids.insert(call_id.as_str().to_string());
            }
        }
        if let Some(assistant) = &turn.assistant {
            if let Some(call_id) = &assistant.call_id {
                ids.insert(call_id.as_str().to_string());
            }
        }
        for call in &turn.calls {
            ids.insert(call.call_id.as_str().to_string());
        }
    }
    ids
}

/// Count user-visible dialogue turns (Dialogue kind with user text).
pub fn story_user_turn_count(story: &Story) -> u64 {
    story
        .turns
        .iter()
        .filter(|t| t.kind == TurnKind::Dialogue && t.user.is_some())
        .count() as u64
}

fn story_snapshots_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("story_snapshots.json")
}

pub fn persist_story_snapshots(storage: &Path, stories: &HashMap<String, Story>) -> Result<()> {
    if stories.is_empty() {
        return Ok(());
    }
    let dir = storage.join(".capture");
    std::fs::create_dir_all(&dir).context("create .capture")?;
    let path = story_snapshots_path(storage);
    let turn_counts = stories
        .iter()
        .map(|(stem, story)| (stem.clone(), story_user_turn_count(story)))
        .collect();
    let file = StorySnapshotsFile {
        updated_at: chrono::Utc::now().to_rfc3339(),
        stories: stories.clone(),
        turn_counts,
    };
    let json = serde_json::to_string_pretty(&file).context("serialize story snapshots")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn load_story_snapshots(storage: &Path) -> Result<HashMap<String, Story>> {
    let path = story_snapshots_path(storage);
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let file: StorySnapshotsFile =
        serde_json::from_str(&raw).context("parse story_snapshots.json")?;
    Ok(file.stories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::sink::{llm_request_summary_record, llm_response_record_with_content};
    use crate::Call;

    fn sample_call(id: &str) -> Call {
        Call {
            call_id: id.into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn rebuild_story_counts_dialogue_turns() {
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("run".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let resp = llm_response_record_with_content(
            Some("run".into()),
            Some("agent".into()),
            200,
            &serde_json::json!({}),
            false,
            Some("ok".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let story = rebuild_session_story("run", "run", &[req, resp]);
        assert_eq!(story_user_turn_count(&story), 1);
        assert!(story_call_ids(&story).contains("c1"));
    }

    #[test]
    fn rebuild_multi_turn_session_story() {
        let mut records = Vec::new();
        for (i, text) in [("c1", "first"), ("c2", "second")] {
            let call = sample_call(i);
            records.push(llm_request_summary_record(
                Some("run".into()),
                Some("agent".into()),
                "m",
                "/v1/chat/completions",
                10,
                "chat",
                "openai",
                Some(text.into()),
                None,
                &call,
                CaptureLevel::Dialogue,
                None,
            ));
            records.push(llm_response_record_with_content(
                Some("run".into()),
                Some("agent".into()),
                200,
                &serde_json::json!({}),
                false,
                Some(format!("reply-{text}")),
                &call,
                CaptureLevel::Dialogue,
            ));
        }
        let story = rebuild_session_story("run", "run", &records);
        assert_eq!(story_user_turn_count(&story), 2);
        assert_eq!(story.turns.len(), 2);
        assert_eq!(story_call_ids(&story).len(), 2);
    }

    #[test]
    fn subagent_session_uses_root_scoped_story_id() {
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("agent-sub".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            Some("sub".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let story = rebuild_session_story("agent-sub", "run-root", &[req]);
        assert_eq!(story.story_id.as_str(), "run-root|agent-sub");
        assert_eq!(story.run_id.as_ref().map(|r| r.as_str()), Some("run-root"));
    }

    #[test]
    fn story_user_turn_count_ignores_autonomous_turn() {
        let call = sample_call("tool-only");
        let req = llm_request_summary_record(
            Some("run".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            None,
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let story = rebuild_session_story("run", "run", &[req]);
        assert_eq!(story.turns.len(), 1);
        assert_eq!(story.turns[0].kind, TurnKind::Autonomous);
        assert_eq!(story_user_turn_count(&story), 0);
    }

    #[test]
    fn persist_and_load_story_snapshots_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let call = sample_call("c1");
        let req = llm_request_summary_record(
            Some("sess".into()),
            Some("agent".into()),
            "m",
            "/v1/chat/completions",
            10,
            "chat",
            "openai",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        let story = rebuild_session_story("sess", "sess", &[req]);
        let mut stories = HashMap::new();
        stories.insert("sess".to_string(), story.clone());

        persist_story_snapshots(dir.path(), &stories).unwrap();
        assert!(story_snapshots_path(dir.path()).is_file());

        let loaded = load_story_snapshots(dir.path()).unwrap();
        assert_eq!(loaded.get("sess").unwrap().turns.len(), story.turns.len());
        assert_eq!(loaded.get("sess").unwrap().agent_id, story.agent_id);
    }

    #[test]
    fn persist_empty_snapshots_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        persist_story_snapshots(dir.path(), &HashMap::new()).unwrap();
        assert!(!story_snapshots_path(dir.path()).exists());
    }
}
