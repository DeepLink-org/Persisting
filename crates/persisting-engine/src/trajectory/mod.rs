//! Trajectory storage: **Lance** and/or session markdown (`0001.md`).
//!
//! Path: `{storage}/{agent_id}/{session_id}/`.
//!
//! Physical backends implement [`TrajectoryStore`] (see [`store`] module).

use std::path::{Path, PathBuf};

use anyhow::Result;
use lance::Dataset;
use lance::Error as LanceError;
pub use persisting_proto::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryReplayRequest,
    TrajectoryReplayResponse, TrajectoryStatsRequest, TrajectoryStatsResponse,
    TrajectoryStorageFormat,
};

pub mod path;
mod storage;
pub mod store;

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
pub const TRAJECTORY_RECORD_COL: &str = "record_ron";

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

/// Directory for a session (`…/storage/agent_id/[root/][subagents/]session_id`).
pub fn trajectory_dataset_dir(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> Result<PathBuf> {
    validate_storage(storage)?;
    let a = validate_path_segment(agent_id, "agent_id")?;
    let s = validate_path_segment(session_id, "session_id")?;
    match root_session_id {
        None => Ok(Path::new(storage).join(a).join(s)),
        Some(root) => {
            let r = validate_path_segment(root, "root_session_id")?;
            if s == r {
                Ok(Path::new(storage).join(a).join(r))
            } else {
                Ok(Path::new(storage).join(a).join(r).join("subagents").join(s))
            }
        }
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

    let mut notes = Vec::new();
    for backend in stores_for_append(fmt) {
        let outcome = backend.append(&session, &lines).await?;
        notes.push(outcome.note);
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
        assert!(nested.ends_with("agent/root-1/subagents/sub-1"));
        let root = trajectory_dataset_dir("/tmp", "agent", "root-1", Some("root-1")).unwrap();
        assert!(root.ends_with("agent/root-1"));
    }

    #[tokio::test]
    async fn append_replay_stats_lance_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sess_1".into(),
            root_session_id: None,
            records_ronl: "(step:1)\n(step:2)\n".into(),
            storage_format: TrajectoryStorageFormat::Lance,
        })
        .await
        .unwrap();
        assert_eq!(append.accepted_records, 2);

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
    }

    #[tokio::test]
    async fn append_replay_stats_nested_lance_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("traj_store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();

        let append = append_async(TrajectoryAppendRequest {
            storage: storage_s.clone(),
            agent_id: "agent_a".into(),
            session_id: "sub-1".into(),
            root_session_id: Some("root-1".into()),
            records_ronl: "(step:1)\n(step:2)\n(step:3)\n".into(),
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
        assert!(st.dataset.contains("root-1/subagents/sub-1"));
    }

    #[tokio::test]
    async fn append_replay_stats_markdown_roundtrip() {
        use persisting_capture::capture_call::CaptureCall;
        use persisting_capture::record::record_to_engine_line;
        use persisting_capture::sink::{llm_request_record, llm_response_record};

        let call = CaptureCall {
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

        let md_path = md::session_markdown_write_path(
            &trajectory_dataset_dir(&storage_s, "a", "s", None).unwrap(),
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
    async fn append_both_writes_lance_and_markdown() {
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

        let root = trajectory_dataset_dir(&storage_s, "a", "s", None).unwrap();
        let md_path = md::session_markdown_write_path(&root);
        assert!(md_path.exists());
        let md_text = std::fs::read_to_string(&md_path).unwrap();
        assert!(md_text.contains("persisting:block"));
        assert!(md_text.contains("hi"));
        assert!(open_trajectory(root.to_string_lossy().as_ref())
            .await
            .unwrap()
            .is_some());
    }
}
