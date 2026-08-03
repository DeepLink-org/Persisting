//! Lance event log backend (canonical trajectory store, schema v1).
//!
//! Capture runs use one run-level dataset at `{run}/events.lance`; `session_id`
//! filters rows when replaying one story view.

use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;

use crate::EventRow;
use anyhow::{Context, Result};
use futures::TryStreamExt;
use lance::dataset::{InsertBuilder, WriteMode, WriteParams};
use lance::deps::arrow_array::RecordBatch;
use lance::io::ObjectStore;
use lance::Dataset;
use lance::Error as LanceError;

use super::lance_rows::{
    event_rows_from_batch, event_rows_to_batch, reassign_global_seq, replay_records_from_batch,
    rows_for_events, rows_for_lines, schema_columns_note, trajectory_arrow_schema,
};
use super::{session_lance_path, AppendOutcome, ReplayOutcome, TrajectorySession, TrajectoryStats};

fn write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn dataset_exists(uri: &str) -> Result<bool> {
    match Dataset::open(uri).await {
        Ok(_) => Ok(true),
        Err(LanceError::DatasetNotFound { .. }) => Ok(false),
        Err(e) => Err(anyhow::anyhow!("{:#}", e)),
    }
}

async fn read_all_rows(uri: &str) -> Result<Vec<EventRow>> {
    if !dataset_exists(uri).await? {
        return Ok(Vec::new());
    }
    let ds = Dataset::open(uri)
        .await
        .with_context(|| format!("open trajectory Lance dataset {uri}"))?;
    let stream = ds
        .scan()
        .try_into_stream()
        .await
        .with_context(|| format!("scan trajectory Lance dataset {uri}"))?;
    let batches: Vec<RecordBatch> = stream
        .try_collect()
        .await
        .with_context(|| format!("collect trajectory Lance batches {uri}"))?;
    let mut rows = Vec::new();
    for batch in &batches {
        rows.extend(event_rows_from_batch(batch)?);
    }
    Ok(rows)
}

async fn write_all_rows(uri: &str, rows: &[EventRow]) -> Result<()> {
    if rows.is_empty() {
        if dataset_exists(uri).await? {
            let (store, path) = ObjectStore::from_uri(uri)
                .await
                .with_context(|| format!("open object store for {uri}"))?;
            store
                .remove_dir_all(path)
                .await
                .with_context(|| format!("remove empty Lance dataset {uri}"))?;
        }
        return Ok(());
    }
    let schema = trajectory_arrow_schema();
    let batch = event_rows_to_batch(schema, rows)?;
    if !is_object_store_uri(uri) {
        if let Some(parent) = Path::new(uri).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
    }
    let mode = if dataset_exists(uri).await? {
        WriteMode::Overwrite
    } else {
        WriteMode::Create
    };
    InsertBuilder::new(uri)
        .with_params(&WriteParams {
            mode,
            ..Default::default()
        })
        .execute(vec![batch])
        .await
        .with_context(|| format!("write trajectory Lance dataset {uri}"))?;
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
    Ok(session_lance_path(session)?.to_string_lossy().into_owned())
}

pub async fn distinct_session_ids_in_run(run: &TrajectorySession) -> Result<Vec<String>> {
    let path = session_lance_path(run)?;
    let uri = path.to_string_lossy().into_owned();
    if !dataset_exists(&uri).await? {
        return Ok(Vec::new());
    }
    let rows = read_all_rows(&uri).await?;
    let mut ids: Vec<String> = rows.into_iter().filter_map(|row| row.session_id).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub async fn exists(session: &TrajectorySession) -> Result<bool> {
    let path = session_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    if !dataset_exists(&uri).await? {
        return Ok(false);
    }
    Ok(!filter_session_rows(read_all_rows(&uri).await?, session).is_empty())
}

pub async fn overwrite_session_lines(
    session: &TrajectorySession,
    lines: &[String],
) -> Result<usize> {
    let records = crate::decode_event_lines(lines)?;
    overwrite_session_events(session, &records).await
}

pub async fn overwrite_session_events(
    session: &TrajectorySession,
    records: &[crate::EventRecord],
) -> Result<usize> {
    let _guard = write_lock().lock().await;
    let path = session_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let replacement_rows = rows_for_events(&session.session_id, 0, records)?;
    let mut merged = Vec::new();
    let existing = read_all_rows(&uri).await?;
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
    write_all_rows(&uri, &merged).await?;
    Ok(records.len())
}

pub async fn append(session: &TrajectorySession, lines: &[String]) -> Result<AppendOutcome> {
    let _guard = write_lock().lock().await;
    let path = session_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    let accepted = lines.len();
    let base_seq = if dataset_exists(&uri).await? {
        Dataset::open(&uri)
            .await
            .with_context(|| format!("open trajectory Lance dataset {uri}"))?
            .count_rows(None)
            .await
            .context("count trajectory Lance rows")? as i64
    } else {
        0
    };
    let new_rows = rows_for_lines(&session.session_id, base_seq, lines)?;
    if new_rows.is_empty() {
        return Ok(AppendOutcome {
            accepted_records: 0,
            persisted_units: 0,
            note: format!(
                "Lance v1: 0 row(s) at {uri} (columns: {})",
                schema_columns_note()
            ),
        });
    }
    let batch = event_rows_to_batch(trajectory_arrow_schema(), &new_rows)?;
    if !is_object_store_uri(&uri) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
    }
    if dataset_exists(&uri).await? {
        let ds = Arc::new(
            Dataset::open(&uri)
                .await
                .with_context(|| format!("open trajectory Lance dataset {uri}"))?,
        );
        InsertBuilder::new(ds)
            .with_params(&WriteParams {
                mode: WriteMode::Append,
                ..Default::default()
            })
            .execute(vec![batch])
            .await
            .with_context(|| format!("append trajectory Lance dataset {uri}"))?;
    } else {
        InsertBuilder::new(uri.as_str())
            .execute(vec![batch])
            .await
            .with_context(|| format!("create trajectory Lance dataset {uri}"))?;
    }
    Ok(AppendOutcome {
        accepted_records: accepted,
        persisted_units: accepted,
        note: format!(
            "Lance v1: {} row(s) at {} (columns: {})",
            accepted,
            uri,
            schema_columns_note()
        ),
    })
}

fn is_object_store_uri(uri: &str) -> bool {
    uri.contains("://")
}

pub async fn replay(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<ReplayOutcome> {
    let path = session_lance_path(session)?;
    let uri = path.to_string_lossy().into_owned();
    if !dataset_exists(&uri).await? {
        anyhow::bail!("trajectory Lance dataset does not exist at {uri}");
    }
    let mut rows = filter_session_rows(read_all_rows(&uri).await?, session);
    if offset > 0 {
        rows = rows.into_iter().skip(offset).collect();
    }
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    let schema = trajectory_arrow_schema();
    let batch = event_rows_to_batch(schema, &rows)?;
    let records = replay_records_from_batch(&batch)?;
    Ok(ReplayOutcome {
        records,
        note: format!(
            "Replay Lance v1 at {uri}: session_id={}, ordered by 'seq', offset={offset}, limit={limit:?}.",
            session.session_id,
        ),
    })
}

pub async fn stats(session: &TrajectorySession) -> Result<TrajectoryStats> {
    let path = session_lance_path(session)?;
    let display = path.to_string_lossy().into_owned();
    if !dataset_exists(&display).await? {
        return Ok(TrajectoryStats {
            dataset: display,
            row_count: 0,
            manifest_version: None,
            status: "missing".to_string(),
            note: "No Lance event log at this path yet; use trajectory add first.".to_string(),
        });
    }
    let rows = filter_session_rows(read_all_rows(&display).await?, session);
    Ok(TrajectoryStats {
        dataset: display.clone(),
        row_count: rows.len(),
        manifest_version: None,
        status: "ok".to_string(),
        note: format!(
            "Lance v1 [{}]; session_id={}; dataset={}",
            schema_columns_note(),
            session.session_id,
            display
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventRecord, StoryCoords};
    use std::sync::atomic::{AtomicU64, Ordering};

    const CHUNK_ROWS: usize = 8192;
    static NEXT_REMOTE_STORE: AtomicU64 = AtomicU64::new(1);

    fn remote_storage(label: &str) -> String {
        format!(
            "shared-memory://pchronicle-events-{}-{label}-{}/trajectories",
            std::process::id(),
            NEXT_REMOTE_STORE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn note_line(content: &str) -> String {
        ron::to_string(
            &serde_json::to_value(EventRecord {
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
            .unwrap(),
        )
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
    async fn append_creates_lance_dataset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        append(&session, &[note_line("one")]).await.unwrap();

        let path = session_lance_path(&session).unwrap();
        assert!(
            dataset_exists(&path.to_string_lossy()).await.unwrap(),
            "committed Lance dataset should exist"
        );
    }

    #[tokio::test]
    async fn append_then_append_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        std::fs::create_dir_all(&storage).unwrap();
        let storage_s = storage.to_string_lossy().to_string();
        let session = flat_session(&storage_s, "agent", "sess");

        append(&session, &[note_line("first")]).await.unwrap();
        append(&session, &[note_line("second")]).await.unwrap();

        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 2);
        assert_eq!(payload_content(&replay.records[0]), "first");
        assert_eq!(payload_content(&replay.records[1]), "second");
    }

    #[tokio::test]
    async fn object_store_uri_supports_append_replay_and_overwrite() {
        let storage = remote_storage("round-trip");
        let session = flat_session(&storage, "agent", "remote-session");
        assert!(display_path(&session).unwrap().starts_with(&storage));

        append(&session, &[note_line("first"), note_line("second")])
            .await
            .unwrap();
        assert_eq!(replay(&session, 0, None).await.unwrap().records.len(), 2);

        overwrite_session_lines(&session, &[note_line("replacement")])
            .await
            .unwrap();
        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 1);
        assert_eq!(payload_content(&replay.records[0]), "replacement");
    }

    #[tokio::test]
    async fn object_store_append_failure_preserves_committed_rows() {
        let storage = remote_storage("failed-append");
        let session = flat_session(&storage, "agent", "session");
        append(&session, &[note_line("committed")]).await.unwrap();

        let error = append(&session, &["this is not valid RON".into()])
            .await
            .unwrap_err();
        assert!(!error.to_string().is_empty());

        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 1);
        assert_eq!(payload_content(&replay.records[0]), "committed");
    }

    #[tokio::test]
    async fn object_store_empty_overwrite_removes_only_target_partition() {
        let storage = remote_storage("empty-overwrite");
        let root = "run";
        let main = run_session(&storage, "agent", root, root);
        let sub = run_session(&storage, "agent", "sub", root);
        append(&main, &[note_line("main")]).await.unwrap();
        append(&sub, &[note_line("sub")]).await.unwrap();

        overwrite_session_events(&sub, &[]).await.unwrap();
        assert!(!exists(&sub).await.unwrap());
        assert_eq!(replay(&main, 0, None).await.unwrap().records.len(), 1);

        overwrite_session_events(&main, &[]).await.unwrap();
        assert!(!exists(&main).await.unwrap());
        assert_eq!(stats(&main).await.unwrap().status, "missing");
        assert!(replay(&main, 0, None).await.is_err());
    }

    #[tokio::test]
    async fn concurrent_object_store_appends_do_not_lose_rows() {
        let storage = remote_storage("concurrent");
        let session = flat_session(&storage, "agent", "session");
        let writes = (0..8).map(|index| {
            let session = session.clone();
            async move { append(&session, &[note_line(&format!("event-{index}"))]).await }
        });
        for result in futures::future::join_all(writes).await {
            result.unwrap();
        }

        let replay = replay(&session, 0, None).await.unwrap();
        assert_eq!(replay.records.len(), 8);
        let mut contents = replay
            .records
            .iter()
            .map(|record| payload_content(record))
            .collect::<Vec<_>>();
        contents.sort();
        assert_eq!(
            contents,
            (0..8)
                .map(|index| format!("event-{index}"))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn session_partition_replay_isolates_stories_in_shared_run_dataset() {
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

        let lance_path = session_lance_path(&main).unwrap();
        assert_eq!(
            session_lance_path(&sub_a).unwrap(),
            lance_path,
            "run-level sessions share one events.lance"
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

        let all_rows = read_all_rows(&session_lance_path(&main).unwrap().to_string_lossy())
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
    async fn append_uses_dataset_wide_seq_base_across_partitions() {
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

        let rows = read_all_rows(&session_lance_path(&main).unwrap().to_string_lossy())
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[tokio::test]
    async fn large_append_produces_valid_lance_dataset() {
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
