//! Trajectory storage: **Lance** (canonical event log) + **TLV Markdown** (optional materialized view).
//! Append/truncate write **one** layer per `--storage-format`; `materialize` is Lance → Markdown only.
//!
//! Path: `{storage}/{agent_id}/{run_id}/` with `{session_id}.md` per logical session.
//!
//! Physical backends implement [`TrajectoryStore`] (see [`store`] module).

use std::path::PathBuf;

use anyhow::Result;
use lance::Dataset;
use lance::Error as LanceError;
pub use persisting_proto::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryExtractRequest,
    TrajectoryExtractResponse, TrajectoryMaterializeRequest, TrajectoryMaterializeResponse,
    TrajectoryReplayRequest, TrajectoryReplayResponse, TrajectoryStatsRequest,
    TrajectoryStatsResponse, TrajectoryStorageFormat, TrajectoryTruncateRequest,
    TrajectoryTruncateResponse,
};

mod convert;
pub mod path;
mod storage;
pub mod store;

pub use storage::{detect_story_primary_layer, story_stats_note};

pub use convert::{
    compact_markdown_to_lance, layer_stats, materialize_lance_to_markdown, CompactOutcome,
    LayerStats, MaterializeOutcome,
};
pub use path::{
    merge_story_location, merge_traj_location, resolve_story_read_location,
    resolve_traj_read_location, try_infer_story_location, try_infer_traj_location, StoryCoords,
    StoryCoords as TrajLocation, StoryLocationPartial, StoryLocationPartial as TrajLocationPartial,
};
pub use persisting_capture::egress::{export_story_bundle, parse_engine_records, ExportOutcome};
pub use persisting_capture::story_coords::{
    story_lance_dataset_dir as trajectory_dataset_dir, story_run_dir as trajectory_run_dir,
};

/// Flat layout `{storage}/{agent_id}/{session_id}/` (no nested run).
pub fn trajectory_dataset_dir_flat(
    storage: &str,
    agent_id: &str,
    session_id: &str,
) -> Result<PathBuf> {
    trajectory_dataset_dir(storage, agent_id, session_id, None)
}
pub use store::{
    store_for_append, store_for_read, LanceTrajectoryStore, MarkdownTrajectoryStore,
    TrajectoryAppendOutcome, TrajectoryReplayOutcome, TrajectorySession, TrajectoryStatsOutcome,
    TrajectoryStore,
};

pub const TRAJECTORY_SEQ_COL: &str = "seq";
pub const TRAJECTORY_TIMESTAMP_COL: &str = "timestamp";
pub const TRAJECTORY_SOURCE_COL: &str = "source";
pub const TRAJECTORY_KIND_COL: &str = "kind";
pub const TRAJECTORY_SESSION_ID_COL: &str = "session_id";
pub const TRAJECTORY_AGENT_ID_COL: &str = "agent_id";
pub const TRAJECTORY_CALL_ID_COL: &str = "call_id";
pub const TRAJECTORY_PARENT_CALL_ID_COL: &str = "parent_call_id";
pub const TRAJECTORY_MODEL_COL: &str = "model";
pub const TRAJECTORY_TRACE_ID_COL: &str = "trace_id";
pub const TRAJECTORY_PAYLOAD_JSON_COL: &str = "payload_json";

/// Lance trajectory schema v1 columns.
pub const TRAJECTORY_V1_COLS: &[&str] = &[
    TRAJECTORY_SEQ_COL,
    TRAJECTORY_TIMESTAMP_COL,
    TRAJECTORY_KIND_COL,
    TRAJECTORY_SOURCE_COL,
    TRAJECTORY_AGENT_ID_COL,
    TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_CALL_ID_COL,
    TRAJECTORY_TRACE_ID_COL,
    TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_MODEL_COL,
    TRAJECTORY_PAYLOAD_JSON_COL,
];

pub fn dataset_uri_display(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<String> {
    Ok(
        trajectory_dataset_dir(storage, agent_id, session_id, root_session_id)?
            .to_string_lossy()
            .into_owned(),
    )
}

pub(crate) async fn open_trajectory(uri: &str) -> Result<Option<Dataset>> {
    match Dataset::open(uri).await {
        Ok(ds) => Ok(Some(ds)),
        Err(LanceError::DatasetNotFound { .. }) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

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
        lance_rows: outcome.stats.source_events,
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
    let lines = parse_engine_records(&request.records_ronl)?;
    let accepted = lines.len();

    if accepted == 0 {
        return Ok(TrajectoryAppendResponse {
            dataset: storage::dataset_display(
                &request.storage,
                &request.agent_id,
                &request.session_id,
                root_session_id,
                request.storage_format,
            )?,
            storage: request.storage,
            agent_id: request.agent_id,
            session_id: request.session_id,
            accepted_records: 0,
            status: "ok".to_string(),
            note: "No non-empty records; storage unchanged.".to_string(),
        });
    }

    let fmt = storage::resolve_for_append(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
        request.storage_format,
    )
    .await?;

    let store = store_for_append(fmt);
    let outcome = store.append(&session, &lines).await?;

    Ok(TrajectoryAppendResponse {
        dataset: storage::dataset_display(
            &request.storage,
            &request.agent_id,
            &request.session_id,
            root_session_id,
            fmt,
        )?,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        accepted_records: accepted,
        status: "ok".to_string(),
        note: format!(
            "storage_format={}. {}",
            storage::format_label(fmt),
            outcome.note
        ),
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

    let fmt = storage::resolve_for_read_with_root(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
        request.storage_format,
    )
    .await?;

    let backend = store_for_read(fmt);
    let outcome = backend
        .replay(&session, request.offset, request.limit)
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

    match request.storage_format {
        TrajectoryStorageFormat::Auto | TrajectoryStorageFormat::Both => {
            stats_dual_layer(&session, &request).await
        }
        fmt => {
            let resolved = storage::resolve_for_read_with_root(
                &request.storage,
                &request.agent_id,
                &request.session_id,
                root_session_id,
                fmt,
            )
            .await?;
            let backend = store_for_read(resolved);
            let outcome = backend.stats(&session).await?;
            Ok(TrajectoryStatsResponse {
                dataset: outcome.dataset,
                storage: request.storage,
                agent_id: request.agent_id,
                session_id: request.session_id,
                row_count: outcome.row_count,
                manifest_version: outcome.manifest_version,
                status: outcome.status,
                note: outcome.note,
            })
        }
    }
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

    let lance = LanceTrajectoryStore;
    let uri = lance.display_path(&session)?;
    let outcome = lance.replay(&session, 0, None).await?;
    let total = outcome.records.len();
    let keep = request.keep_rows.min(total);
    let kept: Vec<String> = outcome.records.into_iter().take(keep).collect();

    let persisted = if kept.is_empty() {
        0
    } else {
        store::overwrite_lines(uri.as_str(), &kept).await?
    };

    let note = format!("truncated Lance: kept {persisted}/{total} row(s) at {uri}");

    Ok(TrajectoryTruncateResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        kept_rows: persisted,
        removed_rows: total.saturating_sub(persisted),
        status: "ok".to_string(),
        note,
    })
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

async fn stats_dual_layer(
    session: &StoryCoords,
    request: &TrajectoryStatsRequest,
) -> Result<TrajectoryStatsResponse> {
    let layers = layer_stats(session).await?;
    let primary = storage::detect_story_primary_layer(&layers, session);
    let row_count = match primary {
        TrajectoryStorageFormat::Markdown => layers.markdown_blocks,
        _ => layers.lance_rows,
    };
    let manifest_version = if layers.lance_rows > 0 {
        LanceTrajectoryStore
            .stats(session)
            .await
            .ok()
            .and_then(|o| o.manifest_version)
    } else {
        None
    };
    let status = if row_count > 0 { "ok" } else { "empty" };
    let dataset = match primary {
        TrajectoryStorageFormat::Markdown => layers
            .markdown_path
            .clone()
            .unwrap_or_else(|| layers.lance_uri.clone()),
        _ => layers.lance_uri.clone(),
    };
    Ok(TrajectoryStatsResponse {
        storage: request.storage.clone(),
        agent_id: request.agent_id.clone(),
        session_id: request.session_id.clone(),
        dataset,
        row_count,
        manifest_version,
        status: status.to_string(),
        note: storage::story_stats_note(&layers, primary),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_capture::markdown_trajectory as md;

    #[test]
    fn parse_engine_records_counts() {
        let v = parse_engine_records("(a:1)\n\n(b:2)\n").unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn rejects_bad_segments() {
        assert!(trajectory_dataset_dir("/tmp", "a/b", "s", None).is_err());
        assert!(trajectory_dataset_dir("/tmp", "..", "s", None).is_err());
        let nested = trajectory_dataset_dir("/tmp", "agent", "sub-1", Some("root-1")).unwrap();
        assert!(nested.ends_with("agent/root-1/.lance/sub-1"));
        let root = trajectory_dataset_dir("/tmp", "agent", "root-1", Some("root-1")).unwrap();
        assert!(root.ends_with("agent/root-1/.lance/root-1"));
    }

    #[tokio::test]
    async fn append_replay_stats_lance_roundtrip() {
        use persisting_capture::record::record_to_engine_line;

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let line1 = record_to_engine_line(&persisting_capture::record::CaptureRecord {
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
        let line2 = record_to_engine_line(&persisting_capture::record::CaptureRecord {
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
            storage: storage_s,
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
        use persisting_capture::record::record_to_engine_line;

        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let mk = |content: &str| {
            record_to_engine_line(&persisting_capture::record::CaptureRecord {
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
            storage: storage_s,
            agent_id: "agent_a".into(),
            session_id: "sub-1".into(),
            storage_format: TrajectoryStorageFormat::Lance,
            root_session_id: Some("root-1".into()),
        })
        .await
        .unwrap();
        assert_eq!(st.row_count, 3);
        assert!(st.dataset.contains("root-1/.lance/sub-1"));
    }

    #[tokio::test]
    async fn append_replay_stats_markdown_roundtrip() {
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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

        let md_path = md::session_markdown_write_path_for_key(
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
    async fn append_both_storage_format_writes_lance_only() {
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::llm_request_record;

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
            storage_format: TrajectoryStorageFormat::Both,
        })
        .await
        .unwrap();

        let lance_dir = trajectory_dataset_dir(&storage_s, "a", "s", None).unwrap();
        assert!(open_trajectory(lance_dir.to_string_lossy().as_ref())
            .await
            .unwrap()
            .is_some());
        let md_path = md::session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        assert!(!md_path.exists());
    }

    #[tokio::test]
    async fn append_auto_writes_markdown_when_only_md_layer_exists() {
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::llm_request_record;

        let dir = tempfile::tempdir().unwrap();
        let storage_s = dir.path().to_string_lossy().to_string();
        std::fs::create_dir_all(&storage_s).unwrap();
        let run = trajectory_run_dir(&storage_s, "a", "s", None).unwrap();
        let md_path = md::session_markdown_write_path_for_key(&run, "s");
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
        let lance_dir = trajectory_dataset_dir(&storage_s, "a", "s", None).unwrap();
        assert!(open_trajectory(lance_dir.to_string_lossy().as_ref())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn replay_auto_prefers_lance_when_both_layers_exist() {
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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

        let ds = open_trajectory(
            trajectory_dataset_dir(&storage_s, "a", "s", None)
                .unwrap()
                .to_string_lossy()
                .as_ref(),
        )
        .await
        .unwrap()
        .expect("lance dataset");
        for col in TRAJECTORY_V1_COLS {
            assert!(ds.schema().field(col).is_some(), "missing column {col}");
        }

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
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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
        assert_eq!(layers.lance_rows, 2);
        assert_eq!(layers.markdown_blocks, 2);

        let compact = compact_markdown_to_lance(&session, true).await.unwrap();
        assert_eq!(compact.stats.source_blocks, 2);
        assert_eq!(compact.stats.lance_rows, 2);
    }

    fn note_lines(n: usize) -> String {
        use persisting_capture::record::record_to_engine_line;
        (0..n)
            .map(|i| {
                record_to_engine_line(&persisting_capture::record::CaptureRecord {
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
    async fn truncate_keeps_first_n_lance_rows() {
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
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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
        let md_path = md::session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        let blocks_before = md::block_count(&md_path).unwrap();

        truncate_async(TrajectoryTruncateRequest {
            storage: storage_s.clone(),
            agent_id: "a".into(),
            session_id: "s".into(),
            root_session_id: None,
            keep_rows: 1,
        })
        .await
        .unwrap();

        assert_eq!(md::block_count(&md_path).unwrap(), blocks_before);
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
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::llm_request_record;

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

        let lance_dir = trajectory_dataset_dir(&storage_s, "a", "s", None).unwrap();
        assert!(open_trajectory(lance_dir.to_string_lossy().as_ref())
            .await
            .unwrap()
            .is_none());

        let md_path = md::session_markdown_write_path_for_key(
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
        use persisting_capture::engine::Call;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

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
        assert_eq!(resp.lance_rows, 2);
        assert!(resp.markdown_blocks >= 1);
        assert!(std::path::Path::new(&resp.markdown_path).exists());
    }
}
