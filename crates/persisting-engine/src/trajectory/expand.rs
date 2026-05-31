//! Expand run-bucket locations into per-`session_id` story views (shared Vortex file).

use anyhow::Result;
use persisting_capture::StoryCoords;

use super::store::vortex;

fn is_shared_vortex_run_bucket(loc: &StoryCoords) -> bool {
    loc.root_session_id
        .as_deref()
        .is_some_and(|root| root == loc.session_id.as_str())
}

/// When a location points at a capture run bucket (`session_id == root_session_id`),
/// read `events.vortex` and emit one coordinate per distinct `session_id` partition.
pub async fn expand_story_locations(locations: Vec<StoryCoords>) -> Result<Vec<StoryCoords>> {
    let mut expanded = Vec::new();
    for loc in locations {
        if !is_shared_vortex_run_bucket(&loc) {
            expanded.push(loc);
            continue;
        }
        let run = StoryCoords::new(
            loc.storage.clone(),
            loc.agent_id.clone(),
            loc.session_id.clone(),
            loc.root_session_id.clone(),
        );
        let session_ids = vortex::distinct_session_ids_in_run(&run).await?;
        if session_ids.is_empty() {
            expanded.push(loc);
            continue;
        }
        for session_id in session_ids {
            expanded.push(StoryCoords::new(
                loc.storage.clone(),
                loc.agent_id.clone(),
                session_id,
                loc.root_session_id.clone(),
            ));
        }
    }
    expanded.sort_by(|a, b| {
        (
            a.storage.as_str(),
            a.agent_id.as_str(),
            a.root_session_id.as_deref().unwrap_or(""),
            a.session_id.as_str(),
        )
            .cmp(&(
                b.storage.as_str(),
                b.agent_id.as_str(),
                b.root_session_id.as_deref().unwrap_or(""),
                b.session_id.as_str(),
            ))
    });
    Ok(expanded)
}

pub fn expand_story_locations_blocking(locations: Vec<StoryCoords>) -> Result<Vec<StoryCoords>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(expand_story_locations(locations))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trajectory::store::vortex;
    use persisting_capture::record::{record_to_engine_line, CaptureRecord};

    fn note_line(session_id: &str, content: &str) -> String {
        record_to_engine_line(&CaptureRecord {
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some(session_id.into()),
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({ "content": content }),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn expand_run_bucket_into_vortex_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-expand";
        let run = StoryCoords::new(&storage_s, "agent", root, Some(root.into()));

        vortex::append(&run, &[note_line(root, "lifecycle")])
            .await
            .unwrap();
        vortex::append(
            &StoryCoords::new(&storage_s, "agent", "story-uuid", Some(root.into())),
            &[note_line("story-uuid", "dialogue")],
        )
        .await
        .unwrap();

        let bucket = vec![StoryCoords::new(
            &storage_s,
            "agent",
            root,
            Some(root.into()),
        )];
        let expanded = expand_story_locations(bucket).await.unwrap();
        assert_eq!(expanded.len(), 2);
        assert!(expanded.iter().any(|l| l.session_id == root));
        assert!(expanded.iter().any(|l| l.session_id == "story-uuid"));
    }
}
