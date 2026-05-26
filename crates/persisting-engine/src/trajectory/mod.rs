//! Trajectory storage: **Lance raw event log** (canonical) + **TLV Markdown** (materialized view).
//!
//! Path: `{storage}/{agent_id}/{run_id}/` with `{session_id}.md` per logical session.
//!
//! Physical backends implement [`TrajectoryStore`] (see [`store`] module).

use std::path::{Path, PathBuf};

use anyhow::Result;
use lance::Dataset;
use lance::Error as LanceError;
pub use persisting_proto::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryMaterializeRequest,
    TrajectoryMaterializeResponse, TrajectoryReplayRequest, TrajectoryReplayResponse,
    TrajectoryStatsRequest, TrajectoryStatsResponse, TrajectoryStorageFormat,
};

mod convert;
pub mod path;
mod storage;
pub mod store;

pub use convert::{
    compact_markdown_to_lance, layer_stats, materialize_lance_to_markdown,
    stream_lines_to_markdown, CompactOutcome, LayerStats, MaterializeOutcome,
};
pub use path::{
    merge_traj_location, resolve_traj_read_location, try_infer_traj_location, TrajLocation,
    TrajLocationPartial,
};
pub use store::{
    store_for_read, stores_for_append, LanceTrajectoryStore, MarkdownTrajectoryStore,
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

use persisting_capture::trajectory_convert::materialize_markdown_path;

fn validate_storage(storage: &str) -> Result<()> {
    if storage.trim().is_empty() {
        anyhow::bail!("storage path must not be empty");
    }
    Ok(())
}

/// One path segment: non-empty after trim, no separators or `..`.
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
pub fn trajectory_run_dir(
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
pub fn trajectory_dataset_dir(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    let run = trajectory_run_dir(storage, agent_id, session_id, root_session_id)?;
    match root_session_id {
        Some(_) => {
            let key = validate_path_segment(session_id, "session_id")?;
            Ok(run.join(".lance").join(key))
        }
        None => Ok(run),
    }
}

/// Legacy two-argument form (flat `{agent_id}/{session_id}/`).
pub fn trajectory_dataset_dir_flat(
    storage: &str,
    agent_id: &str,
    session_id: &str,
) -> Result<PathBuf> {
    trajectory_dataset_dir(storage, agent_id, session_id, None)
}

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
        Err(e) if matches!(e, LanceError::DatasetNotFound { .. }) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

fn parse_engine_records(records_ronl: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for (line_number, line) in records_ronl.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _: ron::value::Value = ron::from_str(line).map_err(|err| {
            anyhow::anyhow!("invalid record at line {}: {}", line_number + 1, err)
        })?;
        out.push(line.to_string());
    }
    Ok(out)
}

fn session_from_request(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> TrajectorySession {
    TrajectorySession::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    )
}

fn materializes_markdown_view(fmt: TrajectoryStorageFormat) -> bool {
    matches!(
        fmt,
        TrajectoryStorageFormat::Markdown | TrajectoryStorageFormat::Both
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

    let lance = LanceTrajectoryStore;
    let outcome = lance.append(&session, &lines).await?;
    let mut notes = vec![outcome.note];

    if materializes_markdown_view(fmt) {
        match stream_lines_to_markdown(&session, &lines) {
            Ok(stats) => {
                let md = materialize_markdown_path(
                    &trajectory_run_dir(
                        &request.storage,
                        &request.agent_id,
                        &request.session_id,
                        root_session_id,
                    )?,
                    &request.session_id,
                );
                notes.push(format!(
                    "streamed markdown: {} block(s) from {} event(s), skipped {} at {}",
                    stats.blocks_appended,
                    stats.events_seen,
                    stats.skipped_events,
                    md.display()
                ));
            }
            Err(e) => notes.push(format!("markdown stream skipped: {e:#}")),
        }
    }

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
            notes.join("; ")
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

    let fmt = storage::resolve_for_read_with_root(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
        request.storage_format,
    )
    .await?;

    let backend = store_for_read(fmt);
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

        let line1 = record_to_engine_line(&persisting_capture::CaptureRecord {
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
        let line2 = record_to_engine_line(&persisting_capture::CaptureRecord {
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
            record_to_engine_line(&persisting_capture::CaptureRecord {
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
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};
        use persisting_capture::Call;

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
            records_ronl,
            storage_format: TrajectoryStorageFormat::Markdown,
        })
        .await
        .unwrap();

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
    async fn append_markdown_writes_lance_and_materializes_md() {
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
        let md_path = md::session_markdown_write_path_for_key(
            &trajectory_run_dir(&storage_s, "a", "s", None).unwrap(),
            "s",
        );
        assert!(md_path.exists());
        let md_text = std::fs::read_to_string(&md_path).unwrap();
        assert!(md_text.contains("persisting:block"));
        assert!(md_text.contains("hi"));
        assert!(open_trajectory(lance_dir.to_string_lossy().as_ref())
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn append_replay_structured_lance_llm_columns() {
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};
        use persisting_capture::Call;

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
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};
        use persisting_capture::Call;

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

        let session = TrajectorySession::new(storage_s.clone(), "a", "s", None);
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
}
