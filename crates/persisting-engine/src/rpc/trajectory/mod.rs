//! RPC adapters over pChronicle trajectory services.
//!
//! pChronicle owns **Lance** (canonical event log), **AgenticMD** (optional
//! materialized view), their schemas, and layer conversions. This module maps
//! protocol requests to those pChronicle operations.
//!
//! Path: `{storage}/{agent_id}/{run_id}/` with `{session_id}.md` per logical session.
//!
use anyhow::Result;
use persisting_pchronicle::{export_story_bundle, materialize_lance_to_markdown, StoryCoords};
pub use persisting_proto::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryExtractRequest,
    TrajectoryExtractResponse, TrajectoryJudgeRequest, TrajectoryJudgeResponse,
    TrajectoryJudgeStatsRequest, TrajectoryJudgeStatsResponse, TrajectoryMaterializeRequest,
    TrajectoryMaterializeResponse, TrajectoryReplayRequest, TrajectoryReplayResponse,
    TrajectoryStatsRequest, TrajectoryStatsResponse, TrajectoryStorageFormat,
    TrajectoryTruncateRequest, TrajectoryTruncateResponse,
};

mod judge;
mod judge_stats;
mod storage;

fn session_from_request(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> StoryCoords {
    StoryCoords::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    )
}

pub async fn materialize_async(
    request: TrajectoryMaterializeRequest,
) -> Result<TrajectoryMaterializeResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let outcome = materialize_lance_to_markdown(&session).await?;
    Ok(TrajectoryMaterializeResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        markdown_path: outcome.markdown_path,
        event_rows: outcome.stats.source_events,
        markdown_blocks: outcome.stats.markdown_blocks,
        skipped_events: outcome.stats.skipped_events,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

pub async fn append_async(request: TrajectoryAppendRequest) -> Result<TrajectoryAppendResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let outcome = persisting_pchronicle::append_trajectory(
        &session,
        storage::to_selection(request.storage_format),
        &request.records_ronl,
    )
    .await?;

    Ok(TrajectoryAppendResponse {
        dataset: outcome.dataset,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        accepted_records: outcome.accepted_records,
        status: outcome.status,
        note: outcome.note,
    })
}

pub async fn replay_async(request: TrajectoryReplayRequest) -> Result<TrajectoryReplayResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );

    let outcome = persisting_pchronicle::replay_trajectory(
        &session,
        storage::to_selection(request.storage_format),
        request.offset,
        request.limit,
    )
    .await?;

    Ok(TrajectoryReplayResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        records: outcome.records,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

pub async fn stats_async(request: TrajectoryStatsRequest) -> Result<TrajectoryStatsResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );

    let outcome = persisting_pchronicle::trajectory_stats(
        &session,
        storage::to_selection(request.storage_format),
    )
    .await?;
    Ok(stats_response_with_judge(
        TrajectoryStatsResponse {
            dataset: outcome.dataset,
            storage: request.storage,
            agent_id: request.agent_id,
            session_id: request.session_id,
            row_count: outcome.row_count,
            manifest_version: outcome.manifest_version,
            judge: None,
            status: outcome.status,
            note: outcome.note,
        },
        &session,
    )
    .await)
}

async fn stats_response_with_judge(
    mut response: TrajectoryStatsResponse,
    session: &StoryCoords,
) -> TrajectoryStatsResponse {
    response.judge = Some(judge_stats::session_judge_stats(session).await);
    response
}

pub async fn truncate_async(
    request: TrajectoryTruncateRequest,
) -> Result<TrajectoryTruncateResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );

    let outcome =
        persisting_pchronicle::truncate_lance_session(&session, request.keep_rows).await?;

    Ok(TrajectoryTruncateResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        kept_rows: outcome.kept_rows,
        removed_rows: outcome.removed_rows,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

pub async fn judge_async(request: TrajectoryJudgeRequest) -> Result<TrajectoryJudgeResponse> {
    judge::judge_async(request).await
}

pub async fn judge_stats_async(
    request: TrajectoryJudgeStatsRequest,
) -> Result<TrajectoryJudgeStatsResponse> {
    judge_stats::judge_stats_async(request).await
}

pub async fn extract_async(request: TrajectoryExtractRequest) -> Result<TrajectoryExtractResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let out = std::path::Path::new(&request.out_dir);
    let outcome = export_story_bundle(&session, out, request.include_subagents)?;

    Ok(TrajectoryExtractResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        out_dir: outcome.out_dir,
        files_copied: outcome.files_copied,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_pchronicle::{
        agenticmd_block_count, compact_markdown_to_lance, expand_story_locations, layer_stats,
        resolve_traj_read_location, session_markdown_write_path_for_key,
        story_lance_event_path as trajectory_event_log_path, story_run_dir as trajectory_run_dir,
        EventRecord,
    };

    struct Call {
        call_id: String,
        trace_id: String,
        started_at: String,
    }

    fn event_record(
        kind: &str,
        session_id: Option<String>,
        agent_id: Option<String>,
        payload: serde_json::Value,
    ) -> EventRecord {
        EventRecord {
            seq: 0,
            source: "engine-test".into(),
            kind: kind.into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            session_id,
            agent_id,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload,
        }
    }

    fn llm_request_record(
        session_id: Option<String>,
        agent_id: Option<String>,
        model: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> EventRecord {
        event_record(
            "llm.request",
            session_id,
            agent_id,
            serde_json::json!({"model": model, "path": path, "body": body}),
        )
    }

    fn llm_response_record(
        session_id: Option<String>,
        agent_id: Option<String>,
        status: u16,
        body: &serde_json::Value,
        streaming: bool,
        call: &Call,
    ) -> EventRecord {
        let mut record = event_record(
            if streaming {
                "llm.response.stream"
            } else {
                "llm.response"
            },
            session_id,
            agent_id,
            serde_json::json!({"status": status, "body": body}),
        );
        record.call_id = Some(call.call_id.clone());
        record.trace_id = Some(call.trace_id.clone());
        record.timestamp = Some(call.started_at.clone());
        record
    }

    fn record_to_engine_line(record: &EventRecord) -> anyhow::Result<String> {
        persisting_pchronicle::encode_event_lines(std::slice::from_ref(record))?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("encode event produced no line"))
    }

    #[test]
    fn rejects_bad_segments() {
        assert!(trajectory_event_log_path("/tmp", "a/b", "s", None).is_err());
        assert!(trajectory_event_log_path("/tmp", "..", "s", None).is_err());
        let nested = trajectory_event_log_path("/tmp", "agent", "sub-1", Some("root-1")).unwrap();
        assert!(nested.ends_with("agent/root-1/events.lance"));
        let root = trajectory_event_log_path("/tmp", "agent", "root-1", Some("root-1")).unwrap();
        assert!(root.ends_with("agent/root-1/events.lance"));
    }

    #[tokio::test]
    async fn append_replay_stats_lance_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let line1 = record_to_engine_line(&EventRecord {
            seq: 0,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"step 1"}),
        })
        .unwrap();
        let line2 = record_to_engine_line(&EventRecord {
            seq: 1,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content":"step 2"}),
        })
        .unwrap();

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sess_1".into(),
            root_session_id: None,
            records_ronl: format!("{line1}\n{line2}\n"),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();
        assert_eq!(append.accepted_records, 2);
        assert!(append.note.contains("Lance v1"));
        let lance_path = trajectory_event_log_path(&storage_s, "agent_a", "sess_1", None).unwrap();
        assert!(lance_path.is_dir(), "expected {}", lance_path.display());

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sess_1".into(),
            offset: 0,
            limit: Some(10),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 2);

        let st = stats_async(TrajectoryStatsRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sess_1".into(),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(st.row_count, 2);
        assert!(st.note.contains("Lance v1"));
    }

    #[tokio::test]
    async fn append_replay_stats_nested_lance_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let mk = |content: &str| {
            record_to_engine_line(&EventRecord {
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
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
        };

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sub-1".into(),
            root_session_id: Some("root-1".into()),
            records_ronl: format!("{}\n{}\n{}\n", mk("a"), mk("b"), mk("c")),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();
        assert_eq!(append.accepted_records, 3);

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sub-2".into(),
            root_session_id: Some("root-1".into()),
            records_ronl: format!("{}\n{}\n", mk("x"), mk("y")),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sub-1".into(),
            offset: 1,
            limit: Some(1),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: Some("root-1".into()),
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 1);

        let st = stats_async(TrajectoryStatsRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sub-1".into(),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: Some("root-1".into()),
        })
        .await
        .unwrap();
        assert_eq!(st.row_count, 3);
        assert!(st.dataset.contains("events.lance"));

        let other = replay_async(TrajectoryReplayRequest {
            storage: storage_s,
            agent_id: "agent_a".into(),
            session_id: "sub-2".into(),
            offset: 0,
            limit: None,
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: Some("root-1".into()),
        })
        .await
        .unwrap();
        assert_eq!(other.records.len(), 2);
    }

    #[tokio::test]
    async fn append_replay_stats_markdown_roundtrip() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1/chat",
            &serde_json::json!({"messages":[{"role":"user","content":"first"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"second"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl: records_ronl.clone(),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let session = StoryCoords::new(storage_s.clone(), "a", "s", None);
        materialize_lance_to_markdown(&session).await.unwrap();

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            offset: 1,
            limit: Some(1),
            storage_format: TrajectoryStorageFormat::Markdown,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 1);
        let row: serde_json::Value = serde_json::from_str(&replay.records[0]).unwrap();
        assert_eq!(row["content"], "second");
        assert_eq!(row["role"], "assistant");

        let md_path = session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        let md_text = std::fs::read_to_string(&md_path).unwrap();
        assert!(md_text.contains("<!-- persisting:block"));

        let st = stats_async(TrajectoryStatsRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            storage_format: TrajectoryStorageFormat::Markdown,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(st.row_count, 2);
    }

    #[tokio::test]
    async fn append_lance_storage_format_writes_only_canonical_layer() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let records_ronl = format!("{}\n", record_to_engine_line(&req).unwrap());

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let lance_path = trajectory_event_log_path(&storage_s, "a", "s", None).unwrap();
        assert!(lance_path.is_dir());
        let md_path = session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        assert!(!md_path.exists());
    }

    #[tokio::test]
    async fn append_auto_writes_markdown_when_only_md_layer_exists() {
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();
        let run = trajectory_run_dir(&storage_s, "a", "s", None).unwrap();
        let md_path = session_markdown_write_path_for_key(&run, "s");
        if let Some(parent) = md_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&md_path, "# existing\n").unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"new"}]}),
        );
        let records_ronl = format!("{}\n", record_to_engine_line(&req).unwrap());

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Auto,
        })
        .await
        .unwrap();

        let md_text = std::fs::read_to_string(&md_path).unwrap();
        assert!(md_text.contains("new"));
        let lance_path = trajectory_event_log_path(&storage_s, "a", "s", None).unwrap();
        assert!(!lance_path.exists());
    }

    #[tokio::test]
    async fn replay_auto_reads_lance_when_both_layers_exist() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"lance-wins"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"from-lance"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let session = StoryCoords::new(storage_s.clone(), "a", "s", None);
        materialize_lance_to_markdown(&session).await.unwrap();

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            offset: 0,
            limit: None,
            storage_format: TrajectoryStorageFormat::Auto,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 2);
        let row0: serde_json::Value = serde_json::from_str(&replay.records[0]).unwrap();
        assert_eq!(row0["kind"], "llm.request");
    }

    #[tokio::test]
    async fn stats_auto_reports_both_layers() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"hello"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let session = StoryCoords::new(storage_s.clone(), "a", "s", None);
        materialize_lance_to_markdown(&session).await.unwrap();

        let st = stats_async(TrajectoryStatsRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            storage_format: TrajectoryStorageFormat::Auto,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(st.row_count, 2);
        assert!(st.note.contains("Story stats via lance"));
        assert!(st.note.contains("Markdown 2"));
    }

    #[tokio::test]
    async fn append_replay_structured_lance_llm_columns() {
        let call = Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let req = llm_request_record(
            Some("s".into()),
            Some("a".into()),
            "deepseek-chat",
            "/v1/chat/completions",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            Some("a".into()),
            200,
            &serde_json::json!({
                "choices":[{"message":{"role":"assistant","content":"hello"}}],
                "usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}
            }),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            offset: 0,
            limit: Some(10),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 2);
        let row0: serde_json::Value = serde_json::from_str(&replay.records[0]).unwrap();
        assert_eq!(row0["kind"], "llm.request");
        let row1: serde_json::Value = serde_json::from_str(&replay.records[1]).unwrap();
        assert_eq!(row1["kind"], "llm.response");
        assert_eq!(row1["call_id"], "call-1");
    }

    #[tokio::test]
    async fn materialize_and_compact_two_layer_roundtrip() {
        let call = Call {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let req = llm_request_record(
            Some("s".into()),
            Some("a".into()),
            "m",
            "/v1/chat/completions",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            Some("a".into()),
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"hello"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let session = StoryCoords::new(storage_s.clone(), "a", "s", None);
        let mat = materialize_lance_to_markdown(&session).await.unwrap();
        assert_eq!(mat.stats.source_events, 2);
        assert_eq!(mat.stats.markdown_blocks, 2);
        assert!(std::path::Path::new(&mat.markdown_path).exists());

        let layers = layer_stats(&session).await.unwrap();
        assert_eq!(layers.event_rows, 2);
        assert_eq!(layers.markdown_blocks, 2);

        let compact = compact_markdown_to_lance(&session, true).await.unwrap();
        assert_eq!(compact.stats.source_blocks, 2);
        assert_eq!(compact.stats.event_rows, 2);
    }

    fn note_lines(n: usize) -> String {
        (0..n)
            .map(|i| {
                record_to_engine_line(&EventRecord {
                    seq: 0,
                    source: "test".into(),
                    kind: "note".into(),
                    timestamp: None,
                    session_id: None,
                    agent_id: None,
                    parent_uuid: None,
                    trace_id: None,
                    call_id: None,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: serde_json::json!({ "content": format!("line-{i}") }),
                })
                .unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[tokio::test]
    async fn truncate_keeps_first_n_event_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().join("store").to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl: note_lines(3),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let tr = truncate_async(TrajectoryTruncateRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            keep_rows: 1,
        })
        .await
        .unwrap();
        assert_eq!(tr.kept_rows, 1);
        assert_eq!(tr.removed_rows, 2);
        assert_eq!(tr.status, "ok");

        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            offset: 0,
            limit: None,
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 1);
        let row: serde_json::Value = serde_json::from_str(&replay.records[0]).unwrap();
        assert_eq!(row["kind"], "note");
        assert_eq!(row["payload"]["content"], "line-0");
    }

    #[tokio::test]
    async fn truncate_does_not_modify_markdown_layer() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"u"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"a"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let session = StoryCoords::new(storage_s.clone(), "a", "s", None);
        materialize_lance_to_markdown(&session).await.unwrap();
        let md_path = session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        let blocks_before = agenticmd_block_count(&md_path).unwrap();

        truncate_async(TrajectoryTruncateRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            keep_rows: 1,
        })
        .await
        .unwrap();

        assert_eq!(agenticmd_block_count(&md_path).unwrap(), blocks_before);
        let replay = replay_async(TrajectoryReplayRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            offset: 0,
            limit: None,
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(replay.records.len(), 1);
    }

    #[tokio::test]
    async fn extract_async_copies_flat_session_tree() {
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().join("store").to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();
        let run = trajectory_run_dir(&storage_s, "agent-x", "sess-1", None).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("sess-1.md"), "# exported\n").unwrap();

        let out = dir.path().join("export");
        let resp = extract_async(TrajectoryExtractRequest {
            storage: storage_s,
            agent_id: "agent-x".into(),
            session_id: "sess-1".into(),
            root_session_id: None,
            out_dir: out.to_string_lossy().into_owned(),
            include_subagents: false,
        })
        .await
        .unwrap();

        assert_eq!(resp.status, "ok");
        assert!(resp.files_copied >= 1);
        let copied = out.join("agent-x").join("sess-1").join("sess-1.md");
        assert!(copied.exists(), "expected {}", copied.display());
        assert!(std::fs::read_to_string(&copied)
            .unwrap()
            .contains("exported"));
    }

    #[tokio::test]
    async fn extract_async_include_subagents_copies_subtree() {
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().join("store").to_string_lossy().to_string();
        let run = trajectory_run_dir(&storage_s, "a", "root-1", None).unwrap();
        let sub = run.join("subagents").join("sub-1");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("sub-1.md"), "# sub\n").unwrap();

        let out = dir.path().join("export");
        let resp = extract_async(TrajectoryExtractRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "root-1".into(),
            root_session_id: Some("root-1".into()),
            out_dir: out.to_string_lossy().into_owned(),
            include_subagents: true,
        })
        .await
        .unwrap();

        assert!(resp.files_copied >= 1);
        let copied = out
            .join("a")
            .join("root-1")
            .join("subagents")
            .join("sub-1")
            .join("sub-1.md");
        assert!(copied.exists(), "expected {}", copied.display());
    }

    #[tokio::test]
    async fn append_storage_format_markdown_without_lance() {
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"md-only"}]}),
        );
        let records_ronl = format!("{}\n", record_to_engine_line(&req).unwrap());

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Markdown,
        })
        .await
        .unwrap();
        assert_eq!(append.accepted_records, 1);
        assert!(append.note.contains("markdown"));

        let lance_path = trajectory_event_log_path(&storage_s, "a", "s", None).unwrap();
        assert!(!lance_path.exists());

        let md_path = session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        assert!(md_path.exists());
        assert!(std::fs::read_to_string(&md_path)
            .unwrap()
            .contains("md-only"));
    }

    #[tokio::test]
    async fn materialize_async_rpc_wrapper() {
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"yo"}}]}),
            false,
            &call,
        );
        let records_ronl = format!(
            "{}\n{}\n",
            record_to_engine_line(&req).unwrap(),
            record_to_engine_line(&resp).unwrap()
        );

        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            records_ronl,
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let resp = materialize_async(TrajectoryMaterializeRequest {
            storage: storage_s,
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
        })
        .await
        .unwrap();
        assert_eq!(resp.status, "ok");
        assert_eq!(resp.event_rows, 2);
        assert!(resp.markdown_blocks >= 1);
        assert!(std::path::Path::new(&resp.markdown_path).exists());
    }

    /// End-to-end regression for `traj stats store/{agent}/` scan:
    /// list run buckets → expand lance partitions → stats each session_id.
    #[tokio::test]
    async fn stats_agent_scan_expands_lance_session_partitions() {
        use persisting_pchronicle::list_story_read_locations;

        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        std::fs::create_dir_all(store.join(".capture")).unwrap();
        std::fs::write(store.join(".capture/run_session"), "run-capture").unwrap();
        let agent = "deepseek-proxy";
        let run = "run-capture";
        let header_session = "header-uuid-session";
        let run_dir = store.join(agent).join(run);
        std::fs::create_dir_all(&run_dir).unwrap();
        let storage_s = store.to_string_lossy().to_string();

        let started = event_record(
            "session.started",
            Some(run.into()),
            Some(agent.into()),
            serde_json::json!({
                "action": "started",
                "mode": "capture.run",
                "command": "claude"
            }),
        );
        let call = Call {
            call_id: "c".into(),
            trace_id: "t".into(),
            started_at: "2026-05-31T00:00:00Z".into(),
        };
        let req = llm_request_record(
            Some(header_session.into()),
            Some(agent.into()),
            "m",
            "/v1/chat/completions",
            &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some(header_session.into()),
            Some(agent.into()),
            200,
            &serde_json::json!({"choices":[{"message":{"role":"assistant","content":"hello"}}]}),
            false,
            &call,
        );
        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: agent.into(),
            session_id: run.into(),
            root_session_id: Some(run.into()),
            records_ronl: format!("{}\n", record_to_engine_line(&started).unwrap()),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();
        append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: agent.into(),
            session_id: header_session.into(),
            root_session_id: Some(run.into()),
            records_ronl: format!(
                "{}\n{}\n",
                record_to_engine_line(&req).unwrap(),
                record_to_engine_line(&resp).unwrap(),
            ),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();

        let agent_path = store.join(agent);
        let buckets =
            list_story_read_locations(agent_path.to_str().unwrap().into(), None, None, None)
                .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].session_id, run);

        let expanded = expand_story_locations(buckets).await.unwrap();
        assert_eq!(expanded.len(), 2);

        let mut counts = std::collections::HashMap::new();
        for loc in expanded {
            let st = stats_async(TrajectoryStatsRequest {
                storage: loc.storage.clone(),
                agent_id: loc.agent_id.clone(),
                session_id: loc.session_id.clone(),
                storage_format: TrajectoryStorageFormat::Lance,
                root_session_id: loc.root_session_id.clone(),
            })
            .await
            .unwrap();
            counts.insert(loc.session_id.clone(), st.row_count);
        }
        assert_eq!(counts.get(run), Some(&1));
        assert_eq!(counts.get(header_session), Some(&2));

        let loc = resolve_traj_read_location(
            "trajectory stats",
            store.to_str().unwrap().into(),
            Some(agent.into()),
            Some(run.into()),
            None,
        )
        .unwrap();
        assert_eq!(loc.storage, store.to_str().unwrap());
    }
}
