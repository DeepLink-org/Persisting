//! Vortex event log backend (canonical trajectory store, schema v1).
//!
//! Capture runs use one run-level table at `{run}/events.vortex`; `session_id`
//! filters rows when replaying one story view.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, Schema as ArrowSchema};
use persisting_capture::event_row::EventRow;
use vortex::array::arrow::{ArrowSessionExt, FromArrowArray};
use vortex::array::stream::ArrayStreamExt;
use vortex::array::VortexSessionExecute;
use vortex::file::OpenOptionsSessionExt;
use vortex::file::WriteOptionsSessionExt;
use vortex::session::VortexSession;
use vortex::VortexSessionDefault;

use super::rows::{
    reassign_global_seq, record_batch_from_rows, replay_records_from_batch, rows_for_lines,
    rows_from_batch, schema_columns_note, trajectory_schema,
};
use super::{
    session_vortex_path, TrajectoryAppendOutcome, TrajectoryReplayOutcome, TrajectorySession,
    TrajectoryStatsOutcome,
};

fn vortex_session() -> &'static VortexSession {
    static SESSION: OnceLock<VortexSession> = OnceLock::new();
    SESSION.get_or_init(VortexSession::default)
}

fn vortex_err(e: vortex::error::VortexError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

fn vortex_to_record_batch(
    array: vortex::array::ArrayRef,
    schema: &ArrowSchema,
) -> Result<RecordBatch> {
    let mut ctx = vortex_session().create_execution_ctx();
    let target = Field::new(
        "",
        DataType::Struct(schema.fields.clone()),
        array.dtype().is_nullable(),
    );
    let arrow = vortex_session()
        .arrow()
        .execute_arrow(array, Some(&target), &mut ctx)
        .map_err(vortex_err)?;
    Ok(RecordBatch::from(arrow.as_struct()))
}

async fn read_all_rows(path: &Path) -> Result<Vec<EventRow>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let file = vortex_session()
        .open_options()
        .open_path(path)
        .await
        .map_err(vortex_err)?;
    let array = file
        .scan()
        .map_err(vortex_err)?
        .into_array_stream()
        .map_err(vortex_err)?
        .read_all()
        .await
        .map_err(vortex_err)?;
    if array.len() == 0 {
        return Ok(Vec::new());
    }
    let schema = trajectory_schema();
    let batch = vortex_to_record_batch(array, schema.as_ref())?;
    rows_from_batch(&batch)
}

async fn write_all_rows(path: &PathBuf, rows: &[EventRow]) -> Result<()> {
    if rows.is_empty() {
        if path.is_file() {
            tokio::fs::remove_file(path)
                .await
                .with_context(|| format!("remove empty vortex file {}", path.display()))?;
        }
        return Ok(());
    }
    let schema = trajectory_schema();
    let batch = record_batch_from_rows(schema, rows)?;
    let array = vortex::array::ArrayRef::from_arrow(batch, false).map_err(vortex_err)?;
    let stream = array.to_array_stream();
    let tmp = path.with_extension("vortex.tmp");
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("create {}", tmp.display()))?;
    vortex_session()
        .write_options()
        .write(&mut file, stream)
        .await
        .map_err(vortex_err)?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn filter_session_rows(rows: Vec<EventRow>, session: &TrajectorySession) -> Vec<EventRow> {
    let mut filtered: Vec<_> = rows
        .into_iter()
        .filter(|row| row.session_id.as_deref() == Some(session.session_id.as_str()))
        .collect();
    filtered.sort_by_key(|row| row.seq);
    filtered
}

pub fn display_path(session: &TrajectorySession) -> Result<String> {
    Ok(session_vortex_path(session)?.to_string_lossy().into_owned())
}

pub async fn distinct_session_ids_in_run(run: &TrajectorySession) -> Result<Vec<String>> {
    let path = session_vortex_path(run)?;
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let rows = read_all_rows(&path).await?;
    let mut ids: Vec<String> = rows.into_iter().filter_map(|row| row.session_id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub async fn exists(session: &TrajectorySession) -> Result<bool> {
    let path = session_vortex_path(session)?;
    if !path.is_file() {
        return Ok(false);
    }
    Ok(!filter_session_rows(read_all_rows(&path).await?, session).is_empty())
}

pub async fn overwrite_session_lines(
    session: &TrajectorySession,
    lines: &[String],
) -> Result<usize> {
    let path = session_vortex_path(session)?;
    let replacement_rows = rows_for_lines(&session.session_id, 0, lines)?;
    let mut merged = Vec::new();
    let existing = read_all_rows(&path).await?;
    if !existing.is_empty() {
        let mut inserted_replacement = false;
        for row in existing {
            if row.session_id.as_deref() == Some(session.session_id.as_str()) {
                if !inserted_replacement {
                    merged.extend(replacement_rows.clone());
                    inserted_replacement = true;
                }
                continue;
            }
            merged.push(row);
        }
        if !inserted_replacement {
            merged.extend(replacement_rows);
        }
    } else {
        merged = replacement_rows;
    }
    reassign_global_seq(&mut merged);
    write_all_rows(&path, &merged).await?;
    Ok(lines.len())
}

pub async fn append(
    session: &TrajectorySession,
    lines: &[String],
) -> Result<TrajectoryAppendOutcome> {
    let path = session_vortex_path(session)?;
    let accepted = lines.len();
    let mut rows = read_all_rows(&path).await?;
    let base_seq = rows.len() as i64;
    rows.extend(rows_for_lines(&session.session_id, base_seq, lines)?);
    write_all_rows(&path, &rows).await?;
    Ok(TrajectoryAppendOutcome {
        accepted_lines: accepted,
        persisted_units: accepted,
        note: format!(
            "Vortex v1: {} row(s) at {} (columns: {})",
            accepted,
            path.display(),
            schema_columns_note()
        ),
    })
}

pub async fn replay(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<TrajectoryReplayOutcome> {
    let path = session_vortex_path(session)?;
    if !path.is_file() {
        anyhow::bail!(
            "trajectory Vortex file does not exist at {}",
            path.display()
        );
    }
    let mut rows = filter_session_rows(read_all_rows(&path).await?, session);
    if offset > 0 {
        rows = rows.into_iter().skip(offset).collect();
    }
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    let schema = trajectory_schema();
    let batch = record_batch_from_rows(schema, &rows)?;
    let records = replay_records_from_batch(&batch)?;
    Ok(TrajectoryReplayOutcome {
        records,
        note: format!(
            "Replay Vortex v1 at {}: session_id={}, ordered by 'seq', offset={}, limit={:?}.",
            path.display(),
            session.session_id,
            offset,
            limit
        ),
    })
}

pub async fn stats(session: &TrajectorySession) -> Result<TrajectoryStatsOutcome> {
    let path = session_vortex_path(session)?;
    let display = path.to_string_lossy().into_owned();
    if !path.is_file() {
        return Ok(TrajectoryStatsOutcome {
            dataset: display,
            row_count: 0,
            manifest_version: None,
            status: "missing".to_string(),
            note: "No Vortex event log at this path yet; use trajectory add first.".to_string(),
        });
    }
    let rows = filter_session_rows(read_all_rows(&path).await?, session);
    Ok(TrajectoryStatsOutcome {
        dataset: display.clone(),
        row_count: rows.len(),
        manifest_version: None,
        status: "ok".to_string(),
        note: format!(
            "Vortex v1 [{}]; session_id={}; file={}",
            schema_columns_note(),
            session.session_id,
            display
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_capture::record::{record_to_engine_line, CaptureRecord};
    use persisting_capture::story_coords::StoryCoords;

    const CHUNK_ROWS: usize = 8192;

    fn note_line(content: &str) -> String {
        record_to_engine_line(&CaptureRecord {
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
    }

    fn run_session(storage: &str, agent: &str, session_id: &str, root: &str) -> TrajectorySession {
        StoryCoords::new(storage, agent, session_id, Some(root.to_string()))
    }

    fn flat_session(storage: &str, agent: &str, session_id: &str) -> TrajectorySession {
        StoryCoords::new(storage, agent, session_id, None)
    }

    fn payload_content(replay_json: &str) -> String {
        let v: serde_json::Value = serde_json::from_str(replay_json).unwrap();
        v["payload"]["content"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn staged_commit_leaves_no_tmp_after_successful_write() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        append(&session, &[note_line("one")]).await.unwrap();

        let path = session_vortex_path(&session).unwrap();
        let tmp = path.with_extension("vortex.tmp");
        assert!(path.is_file(), "committed vortex file should exist");
        assert!(
            !tmp.exists(),
            "staged tmp must be renamed away after commit"
        );
    }

    #[tokio::test]
    async fn staged_commit_replaces_stale_tmp_on_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");
        let path = session_vortex_path(&session).unwrap();
        let tmp = path.with_extension("vortex.tmp");

        append(&session, &[note_line("first")]).await.unwrap();
        std::fs::write(&tmp, b"stale partial write").unwrap();

        append(&session, &[note_line("second")]).await.unwrap();

        assert!(!tmp.exists());
        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 2);
        assert_eq!(payload_content(&replay.records[0]), "first");
        assert_eq!(payload_content(&replay.records[1]), "second");
    }

    #[tokio::test]
    async fn session_partition_replay_isolates_stories_in_shared_run_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-20260101";

        let main = run_session(&storage_s, "agent", root, root);
        let sub_a = run_session(&storage_s, "agent", "agent-sub-a", root);
        let sub_b = run_session(&storage_s, "agent", "agent-sub-b", root);

        append(&main, &[note_line("main-1"), note_line("main-2")])
            .await
            .unwrap();
        append(&sub_a, &[note_line("sub-a-1")]).await.unwrap();
        append(&sub_b, &[note_line("sub-b-1"), note_line("sub-b-2")])
            .await
            .unwrap();

        let vortex_path = session_vortex_path(&main).unwrap();
        assert_eq!(
            session_vortex_path(&sub_a).unwrap(),
            vortex_path,
            "run-level sessions share one events.vortex"
        );

        let main_replay = replay(&main, 0, None).await.unwrap();
        assert_eq!(main_replay.records.len(), 2);
        assert!(main_replay
            .records
            .iter()
            .all(|r| payload_content(r).starts_with("main-")));

        let sub_a_replay = replay(&sub_a, 0, None).await.unwrap();
        assert_eq!(sub_a_replay.records.len(), 1);
        assert_eq!(payload_content(&sub_a_replay.records[0]), "sub-a-1");

        let sub_b_replay = replay(&sub_b, 1, Some(1)).await.unwrap();
        assert_eq!(sub_b_replay.records.len(), 1);
        assert_eq!(payload_content(&sub_b_replay.records[0]), "sub-b-2");
    }

    #[tokio::test]
    async fn session_partition_stats_and_exists_respect_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-partition";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-worker", root);
        let empty = run_session(&storage_s, "agent", "agent-never-written", root);

        append(&main, &[note_line("main")]).await.unwrap();
        append(&sub, &[note_line("sub-1"), note_line("sub-2")])
            .await
            .unwrap();

        assert!(exists(&main).await.unwrap());
        assert!(exists(&sub).await.unwrap());
        assert!(!exists(&empty).await.unwrap());

        let main_stats = stats(&main).await.unwrap();
        assert_eq!(main_stats.row_count, 1);

        let sub_stats = stats(&sub).await.unwrap();
        assert_eq!(sub_stats.row_count, 2);
    }

    #[tokio::test]
    async fn overwrite_session_replaces_only_target_partition() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-overwrite";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-sub", root);

        append(&main, &[note_line("main-old")]).await.unwrap();
        append(&sub, &[note_line("sub-keep-1"), note_line("sub-keep-2")])
            .await
            .unwrap();

        overwrite_session_lines(&sub, &[note_line("sub-new")])
            .await
            .unwrap();

        let main_replay = replay(&main, 0, None).await.unwrap();
        assert_eq!(main_replay.records.len(), 1);
        assert_eq!(payload_content(&main_replay.records[0]), "main-old");

        let sub_replay = replay(&sub, 0, None).await.unwrap();
        assert_eq!(sub_replay.records.len(), 1);
        assert_eq!(payload_content(&sub_replay.records[0]), "sub-new");
    }

    #[tokio::test]
    async fn overwrite_session_reassigns_global_seq_across_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-seq";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-sub", root);

        append(&main, &[note_line("m1")]).await.unwrap();
        append(&sub, &[note_line("s-old")]).await.unwrap();

        overwrite_session_lines(&sub, &[note_line("s-new-a"), note_line("s-new-b")])
            .await
            .unwrap();

        let all_rows = read_all_rows(&session_vortex_path(&main).unwrap())
            .await
            .unwrap();
        assert_eq!(all_rows.len(), 3);
        assert_eq!(
            all_rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(all_rows[0].session_id.as_deref(), Some(root));
        assert_eq!(all_rows[1].session_id.as_deref(), Some("agent-sub"));
        assert_eq!(all_rows[2].session_id.as_deref(), Some("agent-sub"));
    }

    #[tokio::test]
    async fn append_uses_file_wide_seq_base_across_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let root = "run-global-seq";

        let main = run_session(&storage_s, "agent", root, root);
        let sub = run_session(&storage_s, "agent", "agent-sub", root);

        append(&main, &[note_line("m1"), note_line("m2")])
            .await
            .unwrap();
        append(&sub, &[note_line("s1")]).await.unwrap();

        let rows = read_all_rows(&session_vortex_path(&main).unwrap())
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[tokio::test]
    async fn large_append_produces_valid_vortex_with_multiple_internal_partitions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "bulk");

        let lines: Vec<String> = (0..CHUNK_ROWS + 50)
            .map(|i| note_line(&format!("row-{i}")))
            .collect();
        let outcome = append(&session, &lines).await.unwrap();
        assert_eq!(outcome.persisted_units, lines.len());

        let st = stats(&session).await.unwrap();
        assert_eq!(st.row_count, lines.len());

        let replay = replay(&session, CHUNK_ROWS, Some(10)).await.unwrap();
        assert_eq!(replay.records.len(), 10);
        assert_eq!(payload_content(&replay.records[0]), "row-8192");
    }
}
