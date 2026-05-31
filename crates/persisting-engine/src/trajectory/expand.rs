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

    /// Regression: capture.run writes `session.started` under run-* while Claude/header
    /// traffic uses a UUID `session_id` in the same `events.vortex`.
    #[tokio::test]
    async fn claude_run_header_session_stats_regression() {
        use persisting_capture::engine::Call;
        use persisting_capture::lifecycle::{session_started_record, CaptureMode};
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().join("store").to_string_lossy().to_string();
        let agent = "deepseek-proxy";
        let run = "run-20260531-030243";
        let header_session = "58867536-af08-41c7-bc72-669e509697b5";
        let run_coords = StoryCoords::new(&storage_s, agent, run, Some(run.into()));

        let started = session_started_record(
            Some(run.into()),
            Some(agent.into()),
            CaptureMode::Run,
            Some("127.0.0.1:19081"),
            Some("claude"),
        );
        let call = Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-05-31T03:02:43Z".into(),
        };
        let mut lines = vec![record_to_engine_line(&started).unwrap()];
        vortex::append(&run_coords, &lines).await.unwrap();
        lines.clear();
        let header_coords = StoryCoords::new(&storage_s, agent, header_session, Some(run.into()));
        for i in 1..=3 {
            let user = format!("turn-{i}");
            let reply = format!("reply-{i}");
            let req = llm_request_record(
                Some(header_session.into()),
                Some(agent.into()),
                "deepseek-v4-pro",
                "/v1/chat/completions",
                &serde_json::json!({
                    "messages": [{"role": "user", "content": user}]
                }),
            );
            let resp = llm_response_record(
                Some(header_session.into()),
                Some(agent.into()),
                200,
                &serde_json::json!({
                    "choices": [{"message": {"role": "assistant", "content": reply}}]
                }),
                false,
                &call,
            );
            lines.push(record_to_engine_line(&req).unwrap());
            lines.push(record_to_engine_line(&resp).unwrap());
        }
        vortex::append(&header_coords, &lines).await.unwrap();

        let run_stats = vortex::stats(&run_coords).await.unwrap();
        assert_eq!(
            run_stats.row_count, 1,
            "run id partition should only see session.started"
        );

        let header_coords = StoryCoords::new(&storage_s, agent, header_session, Some(run.into()));
        let header_stats = vortex::stats(&header_coords).await.unwrap();
        assert_eq!(
            header_stats.row_count, 6,
            "header UUID partition should see all LLM events"
        );

        let bucket = vec![StoryCoords::new(&storage_s, agent, run, Some(run.into()))];
        let expanded = expand_story_locations(bucket).await.unwrap();
        assert_eq!(expanded.len(), 2);

        let mut row_counts = std::collections::HashMap::new();
        for loc in expanded {
            row_counts.insert(
                loc.session_id.clone(),
                vortex::stats(&loc).await.unwrap().row_count,
            );
        }
        assert_eq!(row_counts.get(run), Some(&1));
        assert_eq!(row_counts.get(header_session), Some(&6));
    }
}
