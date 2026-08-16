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

fn note(content: &str) -> EventRecord {
    EventRecord {
        identity: crate::EventIdentity::default(),
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
    }
}

fn identified_note(event_id: &str, seq: u64, content: &str) -> EventRecord {
    EventRecord {
        identity: crate::EventIdentity {
            event_id: Some(event_id.into()),
            ..Default::default()
        },
        seq,
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
    }
}

fn run_session(storage: &str, agent: &str, session_id: &str, root: &str) -> StoryCoords {
    StoryCoords::new(storage, agent, session_id, Some(root.to_string()))
}

fn flat_session(storage: &str, agent: &str, session_id: &str) -> StoryCoords {
    StoryCoords::new(storage, agent, session_id, None)
}

fn payload_content(record: &EventRecord) -> String {
    record.payload["content"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn append_creates_lance_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let session = flat_session(&storage_s, "agent", "sess");

    append_events(&session, &[note("one")]).await.unwrap();

    let path = raw_event_lance_path(&session).unwrap();
    let (manifest, datasets) = open_visible_snapshot(&path.to_string_lossy())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manifest.total_rows(), 1);
    assert_eq!(datasets.len(), 1);
}

#[tokio::test]
async fn append_then_append_preserves_rows() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let session = flat_session(&storage_s, "agent", "sess");

    append_events(&session, &[note("first")]).await.unwrap();
    append_events(&session, &[note("second")]).await.unwrap();

    let replay = replay(&session, 0, None).await.unwrap();
    assert_eq!(replay.records.len(), 2);
    assert_eq!(payload_content(&replay.records[0]), "first");
    assert_eq!(payload_content(&replay.records[1]), "second");
}

#[tokio::test]
async fn append_preserves_conflicting_claims_and_replays_physical_identity() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let session = run_session(storage.to_str().unwrap(), "agent", "child", "run");

    type IdentityMutation = fn(&mut EventRecord);
    let conflicts: [(&str, IdentityMutation); 4] = [
        ("session_id", |record: &mut EventRecord| {
            record.session_id = Some("other".into())
        }),
        ("storyline_id", |record: &mut EventRecord| {
            record.identity.storyline_id = Some("other".into())
        }),
        ("run_id", |record: &mut EventRecord| {
            record.identity.run_id = Some("other".into())
        }),
        ("agent_id", |record: &mut EventRecord| {
            record.agent_id = Some("other".into())
        }),
    ];
    for (field, mutate) in conflicts {
        let mut record = note("conflict");
        mutate(&mut record);
        record.payload["conflict_field"] = serde_json::Value::String(field.into());
        append_events(&session, &[record]).await.unwrap();
    }
    let replayed = replay(&session, 0, None).await.unwrap().records;
    assert_eq!(replayed.len(), conflicts.len());
    assert!(replayed.iter().all(|record| {
        record.session_id.as_deref() == Some("child") && record.agent_id.as_deref() == Some("agent")
    }));

    let uri = raw_event_lance_path(&session)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let rows = read_all_rows(&uri).await.unwrap();
    let preserved = rows
        .iter()
        .map(|row| serde_json::from_str::<EventRecord>(&row.payload_json).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(preserved[0].session_id.as_deref(), Some("other"));
    assert_eq!(preserved[1].identity.storyline_id.as_deref(), Some("other"));
    assert_eq!(preserved[2].identity.run_id.as_deref(), Some("other"));
    assert_eq!(preserved[3].agent_id.as_deref(), Some("other"));
}

#[tokio::test]
async fn append_fills_all_storage_identity_fields() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let session = run_session(storage.to_str().unwrap(), "agent", "child", "run");

    append_events(&session, &[note("canonical")]).await.unwrap();
    let record = replay(&session, 0, None).await.unwrap().records.remove(0);
    assert_eq!(record.session_id.as_deref(), Some("child"));
    assert_eq!(record.identity.storyline_id.as_deref(), Some("child"));
    assert_eq!(record.identity.run_id.as_deref(), Some("run"));
    assert_eq!(record.agent_id.as_deref(), Some("agent"));
}

#[tokio::test]
async fn pinned_source_reads_append_ranges_and_selected_storylines() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let root = run_session(storage.to_str().unwrap(), "agent", "root", "run");
    let child = run_session(storage.to_str().unwrap(), "agent", "child", "run");

    append_events(&root, &[identified_note("root-0", 0, "root-zero")])
        .await
        .unwrap();
    append_events(&child, &[identified_note("child-0", 0, "child-zero")])
        .await
        .unwrap();
    append_events(&root, &[identified_note("root-1", 1, "root-one")])
        .await
        .unwrap();

    let source = RawEventDataSource::open(raw_event_lance_path(&root).unwrap())
        .await
        .unwrap();
    let suffix = source
        .read_records_range_in_append_order(1, 3)
        .await
        .unwrap();
    assert_eq!(
        suffix.iter().map(payload_content).collect::<Vec<_>>(),
        ["child-zero", "root-one"]
    );

    let selected = source
        .read_records_for_storylines(&["root".to_string()].into_iter().collect())
        .await
        .unwrap();
    assert_eq!(
        selected.iter().map(payload_content).collect::<Vec<_>>(),
        ["root-zero", "root-one"]
    );
}

#[tokio::test]
async fn partitioned_append_reports_one_root_failure_without_losing_other_root() {
    let dir = tempfile::tempdir().unwrap();
    let invalid_storage = dir.path().join("not-a-directory");
    std::fs::write(&invalid_storage, b"file").unwrap();
    let invalid = flat_session(invalid_storage.to_str().unwrap(), "agent", "bad");
    let valid = flat_session(dir.path().join("valid").to_str().unwrap(), "agent", "good");
    let invalid_uri = raw_event_lance_path(&invalid)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let valid_uri = raw_event_lance_path(&valid)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let report = RawEventLanceAppender::default()
        .append_event_batch_partitioned(&[(invalid, note("bad")), (valid.clone(), note("good"))])
        .await
        .unwrap();
    assert!(report.outcome_for(&invalid_uri).unwrap().is_err());
    assert_eq!(report.outcome_for(&valid_uri), Some(&Ok(1)));
    assert_eq!(replay(&valid, 0, None).await.unwrap().records.len(), 1);
}

#[tokio::test]
async fn typed_append_preserves_duplicates_and_storyline_seq() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
    let record = identified_note("stable-event", 42, "once");

    let first = append_events(&session, std::slice::from_ref(&record))
        .await
        .unwrap();
    let duplicate = append_events(&session, &[record.clone(), record])
        .await
        .unwrap();

    assert_eq!(first.accepted_records, 1);
    assert_eq!(duplicate.accepted_records, 2);
    let replay = replay(&session, 0, None).await.unwrap();
    assert_eq!(replay.records.len(), 3);
    assert_eq!(
        replay.records[0].identity.event_id.as_deref(),
        Some("stable-event")
    );
    assert_eq!(replay.records[0].seq, 42);
}

#[tokio::test]
async fn missing_event_id_is_stored_as_null_without_deduplication() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
    let mut record = identified_note("temporary", 7, "retry-safe");
    record.identity.event_id = None;

    let first = append_events(&session, std::slice::from_ref(&record))
        .await
        .unwrap();
    let retry = append_events(&session, std::slice::from_ref(&record))
        .await
        .unwrap();
    assert_eq!(first.accepted_records, 1);
    assert_eq!(retry.accepted_records, 1);

    let uri = raw_event_lance_path(&session)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (_, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
    assert!(datasets[0]
        .schema()
        .field(crate::TRAJECTORY_EVENT_ID_COL)
        .is_some());
    let rows = read_all_rows(&raw_event_lance_path(&session).unwrap().to_string_lossy())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].event_id, None);
    assert_eq!(rows[1].event_id, None);
    let restored = replay(&session, 0, None).await.unwrap().records;
    assert!(restored
        .iter()
        .all(|record| record.identity.event_id.is_none()));
}

#[tokio::test]
async fn incompatible_event_schema_is_rejected_without_migration() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let session = flat_session(storage.to_str().unwrap(), "agent", "sess");
    append_events(&session, &[identified_note("event-1", 0, "first")])
        .await
        .unwrap();

    let uri = raw_event_lance_path(&session)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (manifest, _) = open_visible_snapshot(&uri).await.unwrap().unwrap();
    let mut segment = manifest.segments[0].clone();
    let mut dataset = Dataset::open(&raw_event_manifest::segment_uri(&uri, &segment.id))
        .await
        .unwrap();
    dataset
        .drop_columns(&[crate::TRAJECTORY_EVENT_ID_COL])
        .await
        .unwrap();
    segment.version = dataset.version_id();
    raw_event_manifest::publish_segment(&uri, &manifest.active_writer, segment)
        .await
        .unwrap();

    let replay_error = replay(&session, 0, None).await.unwrap_err();
    assert!(replay_error.to_string().contains("schema mismatch"));
}

#[tokio::test]
async fn one_cached_appender_preserves_physical_append_order() {
    let storage = remote_storage("cached-single-writer");
    let session = flat_session(&storage, "agent", "session");
    let mut writer = RawEventLanceAppender::default();

    writer
        .append_event_batch(&[(session.clone(), identified_note("first", 10, "first"))])
        .await
        .unwrap();
    writer
        .append_event_batch(&[(session.clone(), identified_note("second", 20, "second"))])
        .await
        .unwrap();
    writer
        .append_event_batch(&[(session.clone(), identified_note("third", 30, "third"))])
        .await
        .unwrap();
    let reports = writer.finish();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].fragments_removed, 0);

    let uri = raw_event_lance_path(&session)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(datasets[0].get_fragments().len(), 3);
    assert!(datasets[0]
        .load_indices_by_name(SESSION_INDEX_NAME)
        .await
        .unwrap()
        .is_empty());

    let restored = replay(&session, 0, None).await.unwrap();
    assert_eq!(
        restored
            .records
            .iter()
            .map(|record| record.seq)
            .collect::<Vec<_>>(),
        [10, 20, 30]
    );
}

#[tokio::test]
async fn replay_available_follows_committed_pages() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let session = flat_session(&storage_s, "agent", "sess");

    assert!(replay_available(&session, 0, Some(2))
        .await
        .unwrap()
        .is_none());

    append_events(&session, &[note("first"), note("second"), note("third")])
        .await
        .unwrap();
    let first_page = replay_available(&session, 0, Some(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first_page
            .records
            .iter()
            .map(payload_content)
            .collect::<Vec<_>>(),
        ["first", "second"]
    );

    append_events(&session, &[note("fourth")]).await.unwrap();
    let second_page = replay_available(&session, first_page.records.len(), Some(2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        second_page
            .records
            .iter()
            .map(payload_content)
            .collect::<Vec<_>>(),
        ["third", "fourth"]
    );
    assert!(replay_available(&session, 4, Some(2))
        .await
        .unwrap()
        .unwrap()
        .records
        .is_empty());
}

#[tokio::test]
async fn object_store_uri_supports_append_only_replay() {
    let storage = remote_storage("round-trip");
    let session = flat_session(&storage, "agent", "remote-session");
    assert!(display_path(&session).unwrap().starts_with(&storage));

    append_events(&session, &[note("first"), note("second")])
        .await
        .unwrap();
    assert_eq!(replay(&session, 0, None).await.unwrap().records.len(), 2);

    append_events(&session, &[note("third")]).await.unwrap();
    let replay = replay(&session, 0, None).await.unwrap();
    assert_eq!(replay.records.len(), 3);
    assert_eq!(payload_content(&replay.records[2]), "third");
}

#[tokio::test]
async fn object_store_append_failure_preserves_committed_rows() {
    let storage = remote_storage("failed-append");
    let session = flat_session(&storage, "agent", "session");
    append_events(&session, &[note("committed")]).await.unwrap();

    let error = append_events(&session, &[identified_note("invalid", u64::MAX, "invalid")])
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());

    let replay = replay(&session, 0, None).await.unwrap();
    assert_eq!(replay.records.len(), 1);
    assert_eq!(payload_content(&replay.records[0]), "committed");
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

    append_events(&main, &[note("main-1"), note("main-2")])
        .await
        .unwrap();
    append_events(&sub_a, &[note("sub-a-1")]).await.unwrap();
    append_events(&sub_b, &[note("sub-b-1"), note("sub-b-2")])
        .await
        .unwrap();

    let lance_path = raw_event_lance_path(&main).unwrap();
    assert_eq!(
        raw_event_lance_path(&sub_a).unwrap(),
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

    append_events(&main, &[note("main")]).await.unwrap();
    append_events(&sub, &[note("sub-1"), note("sub-2")])
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
async fn append_keeps_producer_seq_independent_across_partitions() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let root = "run-global-seq";

    let main = run_session(&storage_s, "agent", root, root);
    let sub = run_session(&storage_s, "agent", "agent-sub", root);

    append_events(&main, &[note("m1"), note("m2")])
        .await
        .unwrap();
    append_events(&sub, &[note("s1")]).await.unwrap();

    let rows = read_all_rows(&raw_event_lance_path(&main).unwrap().to_string_lossy())
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![0, 0, 0]
    );
}

#[tokio::test]
async fn routed_micro_batch_commits_multiple_stories_in_one_fragment() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let main = run_session(&storage_s, "agent", "run", "run");
    let sub = run_session(&storage_s, "agent", "sub", "run");
    let records = [note("main"), note("sub")];
    RawEventLanceAppender::default()
        .append_event_batch(&[
            (main.clone(), records[0].clone()),
            (sub.clone(), records[1].clone()),
        ])
        .await
        .unwrap();

    assert_eq!(replay(&main, 0, None).await.unwrap().records.len(), 1);
    assert_eq!(replay(&sub, 0, None).await.unwrap().records.len(), 1);
    let uri = raw_event_lance_path(&main)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(visible_fragment_count(&uri).await.unwrap(), 1);
}

#[tokio::test]
async fn routed_batch_creates_one_fragment_per_run_not_per_record() {
    const RUNS: usize = 3;
    const STORIES_PER_RUN: usize = 4;
    const ROWS_PER_STORY: usize = 64;

    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let mut entries = Vec::with_capacity(RUNS * STORIES_PER_RUN * ROWS_PER_STORY);
    let mut run_roots = Vec::with_capacity(RUNS);

    for run_index in 0..RUNS {
        let root = format!("run-{run_index}");
        let root_session = run_session(&storage_s, "agent", &root, &root);
        run_roots.push(root_session);
        for story_index in 0..STORIES_PER_RUN {
            let session = run_session(
                &storage_s,
                "agent",
                &format!("run-{run_index}-story-{story_index}"),
                &root,
            );
            for row_index in 0..ROWS_PER_STORY {
                entries.push((
                    session.clone(),
                    identified_note(
                        &format!("event-{run_index}-{story_index}-{row_index}"),
                        row_index as u64,
                        &format!("row-{row_index}"),
                    ),
                ));
            }
        }
    }

    let outcome = RawEventLanceAppender::default()
        .append_event_batch(&entries)
        .await
        .unwrap();
    assert_eq!(outcome.accepted_records, entries.len());

    for root in run_roots {
        let uri = raw_event_lance_path(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
        assert_eq!(manifest.segments.len(), 1);
        assert_eq!(
            datasets[0].get_fragments().len(),
            1,
            "one routed call should create one fragment for each run dataset"
        );
        assert_eq!(
            datasets[0].count_rows(None).await.unwrap(),
            STORIES_PER_RUN * ROWS_PER_STORY
        );
    }
}

#[tokio::test]
async fn explicit_maintenance_compacts_fragments_and_builds_session_index() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let session = flat_session(&storage_s, "agent", "run");
    for index in 0..4 {
        append_events(&session, &[note(&format!("event-{index}"))])
            .await
            .unwrap();
    }
    let report = maintain(
        &session,
        &LanceMaintenanceOptions {
            vacuum_older_than: Some(Duration::ZERO),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(report.fragments_removed >= 4);
    let uri = raw_event_lance_path(&session)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (manifest, datasets) = open_visible_snapshot(&uri).await.unwrap().unwrap();
    assert_eq!(manifest.segments.len(), 1);
    assert_eq!(datasets[0].get_fragments().len(), 1);
    assert!(!datasets[0]
        .load_indices_by_name(SESSION_INDEX_NAME)
        .await
        .unwrap()
        .is_empty());
    assert!(report.old_versions_removed >= 4);
    let segment_directories =
        std::fs::read_dir(raw_event_lance_path(&session).unwrap().join("segments"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
    assert_eq!(segment_directories, 1);
    assert_eq!(replay(&session, 0, None).await.unwrap().records.len(), 4);
}

#[tokio::test]
async fn large_append_produces_valid_lance_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    std::fs::create_dir_all(&storage).unwrap();
    let storage_s = storage.to_string_lossy().to_string();
    let session = flat_session(&storage_s, "agent", "bulk");

    let records: Vec<EventRecord> = (0..CHUNK_ROWS + 50)
        .map(|i| note(&format!("row-{i}")))
        .collect();
    let outcome = append_events(&session, &records).await.unwrap();
    assert_eq!(outcome.persisted_units, records.len());

    let st = stats(&session).await.unwrap();
    assert_eq!(st.row_count, records.len());

    let replay = replay(&session, CHUNK_ROWS, Some(10)).await.unwrap();
    assert_eq!(replay.records.len(), 10);
    assert_eq!(payload_content(&replay.records[0]), "row-8192");
}
