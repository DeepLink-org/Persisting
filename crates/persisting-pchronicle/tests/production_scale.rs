//! Production-scale invariants for the canonical append-only event path.
//!
//! The regular tests are bounded scale proxies suitable for CI. The ignored
//! stress test can be enlarged without changing source code:
//!
//! ```text
//! PCHRONICLE_STRESS_BATCHES=512 \
//! PCHRONICLE_STRESS_BATCH_ROWS=256 \
//! cargo test -p persisting-pchronicle --test production_scale \
//!   sustained_append_stress -- --ignored --nocapture
//! ```
//!
//! Elapsed time is reported but deliberately not asserted: throughput gates
//! belong on dedicated, controlled benchmark hardware rather than shared CI.

use std::time::Instant;

use anyhow::{Context, Result};
use persisting_agentctl::RunId;
use persisting_pchronicle::{
    raw_event_lance_path, ChronicleQueryEngine, ChronicleQueryExecutionOptions, DocumentFormat,
    EventIdentity, EventRecord, EventWriterFence, LanceMaintenanceOptions, LeaseAcquireOutcome,
    RawEventLanceAppender, RawEventLanceStore, RunControlStore, StoryCoords,
};

const CI_STORIES: usize = 4;
const CI_BATCHES: usize = 24;
const CI_ROWS_PER_STORY_BATCH: usize = 32;

fn run_session(storage: &str, run_id: &str, story_index: usize) -> StoryCoords {
    StoryCoords::new(
        storage,
        "scale-agent",
        format!("{run_id}-story-{story_index}"),
        Some(run_id.to_string()),
    )
}

fn event(story_index: usize, sequence: usize) -> EventRecord {
    // A deliberately repeated ID proves that storage cost and row count do not
    // depend on business-key uniqueness. NULL IDs exercise the same contract.
    let event_id = match sequence % 17 {
        0 => None,
        1 | 2 => Some(format!("story-{story_index}-deliberate-duplicate")),
        _ => Some(format!("story-{story_index}-event-{sequence}")),
    };
    EventRecord {
        identity: EventIdentity {
            event_id,
            ..Default::default()
        },
        seq: sequence as u64,
        source: "production-scale-test".into(),
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
        payload: serde_json::json!({
            "story_index": story_index,
            "sequence": sequence,
        }),
    }
}

fn batch_entries(
    sessions: &[StoryCoords],
    batch_index: usize,
    rows_per_story: usize,
) -> Vec<(StoryCoords, EventRecord)> {
    let first_sequence = batch_index * rows_per_story;
    sessions
        .iter()
        .enumerate()
        .flat_map(|(story_index, session)| {
            (first_sequence..first_sequence + rows_per_story)
                .map(move |sequence| (session.clone(), event(story_index, sequence)))
        })
        .collect()
}

async fn append_batches(
    sessions: &[StoryCoords],
    start_batch: usize,
    batch_count: usize,
    rows_per_story: usize,
) -> Result<()> {
    let mut writer = RawEventLanceAppender::default();
    for batch_index in start_batch..start_batch + batch_count {
        let entries = batch_entries(sessions, batch_index, rows_per_story);
        let outcome = writer.append_event_batch(&entries).await?;
        assert_eq!(outcome.accepted_records, entries.len());
        assert_eq!(outcome.persisted_units, entries.len());
    }
    let reports = writer.finish();
    assert_eq!(reports.len(), 1, "all Storylines belong to one Run");
    assert!(reports[0].final_version.is_some());
    Ok(())
}

async fn replay_all_in_pages(session: &StoryCoords, page_size: usize) -> Result<Vec<EventRecord>> {
    let mut offset = 0;
    let mut records = Vec::new();
    loop {
        let page = RawEventLanceStore
            .read_events(session, offset, Some(page_size))
            .await?;
        if page.is_empty() {
            break;
        }
        offset += page.len();
        records.extend(page);
    }
    Ok(records)
}

fn count_from_jsonl(jsonl: &str) -> Result<u64> {
    let value: serde_json::Value = serde_json::from_str(jsonl.trim())?;
    value["row_count"]
        .as_u64()
        .context("query result row_count is not a u64")
}

#[tokio::test]
async fn sustained_micro_batches_survive_restart_maintenance_and_follow_reads() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let run_id = "scale-run";
    let sessions = (0..CI_STORIES)
        .map(|story_index| run_session(&storage, run_id, story_index))
        .collect::<Vec<_>>();

    // Drop and recreate the cached writer halfway through to model a clean
    // process restart without changing append order or losing acknowledged rows.
    append_batches(&sessions, 0, CI_BATCHES / 2, CI_ROWS_PER_STORY_BATCH).await?;
    append_batches(
        &sessions,
        CI_BATCHES / 2,
        CI_BATCHES - CI_BATCHES / 2,
        CI_ROWS_PER_STORY_BATCH,
    )
    .await?;

    let expected_rows_per_story = CI_BATCHES * CI_ROWS_PER_STORY_BATCH;
    for (story_index, session) in sessions.iter().enumerate() {
        assert_eq!(
            RawEventLanceStore.stats(session).await?.row_count,
            expected_rows_per_story
        );
        let records = replay_all_in_pages(session, 73).await?;
        assert_eq!(records.len(), expected_rows_per_story);
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            (0..expected_rows_per_story as u64).collect::<Vec<_>>(),
            "story {story_index} must preserve physical append order"
        );
        assert!(
            records
                .iter()
                .any(|record| record.identity.event_id.is_none()),
            "NULL event IDs must survive sustained append"
        );
        let duplicate_id = format!("story-{story_index}-deliberate-duplicate");
        let duplicate_count = records
            .iter()
            .filter(|record| record.identity.event_id.as_deref() == Some(duplicate_id.as_str()))
            .count();
        assert!(duplicate_count > CI_BATCHES);
    }

    let before = RawEventLanceStore.layout_stats(&sessions[0]).await?;
    assert_eq!(
        before.visible_fragments, CI_BATCHES,
        "one fragment is expected per micro-batch"
    );
    assert_eq!(before.visible_segments, 2, "the writer was restarted once");

    let report = RawEventLanceStore
        .maintain(
            &sessions[0],
            &LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            },
        )
        .await?;
    assert!(report.fragments_removed >= CI_BATCHES);
    let after = RawEventLanceStore.layout_stats(&sessions[0]).await?;
    assert_eq!(after.visible_segments, 1);
    assert!(
        after.visible_fragments < before.visible_fragments,
        "maintenance must reduce fragment debt"
    );

    // A fresh writer must continue after maintenance without overwriting the
    // compacted snapshot or reordering the existing logical event stream.
    let continuation = event(0, expected_rows_per_story);
    RawEventLanceStore
        .append_events(&sessions[0], std::slice::from_ref(&continuation))
        .await?;
    let records = replay_all_in_pages(&sessions[0], 127).await?;
    assert_eq!(records.len(), expected_rows_per_story + 1);
    let restored = records.last().unwrap();
    assert_eq!(restored.seq, continuation.seq);
    assert_eq!(restored.identity.event_id, continuation.identity.event_id);
    assert_eq!(restored.identity.run_id.as_deref(), Some(run_id));
    assert_eq!(
        restored.identity.storyline_id.as_deref(),
        Some(sessions[0].session_id.as_str())
    );
    assert_eq!(restored.payload, continuation.payload);
    Ok(())
}

#[tokio::test]
async fn event_queries_pin_a_snapshot_while_append_continues() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let session = run_session(&storage, "snapshot-run", 0);
    let sessions = vec![session.clone()];
    let half = 8;

    append_batches(&sessions, 0, half, 64).await?;
    let path = raw_event_lance_path(&session)?;
    let pinned = ChronicleQueryEngine::open(
        DocumentFormat::CanonicalEvent,
        &path,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    append_batches(&sessions, half, half, 64).await?;

    let pinned_count = count_from_jsonl(
        &pinned
            .query_jsonl("SELECT COUNT(*) AS row_count FROM events")
            .await?,
    )?;
    assert_eq!(pinned_count, (half * 64) as u64);

    let current = ChronicleQueryEngine::open(
        DocumentFormat::CanonicalEvent,
        &path,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let current_count = count_from_jsonl(
        &current
            .query_jsonl("SELECT COUNT(*) AS row_count FROM events")
            .await?,
    )?;
    assert_eq!(current_count, (half * 2 * 64) as u64);
    assert_ne!(pinned.backend_info(), current.backend_info());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_runs_append_in_parallel_without_cross_run_contamination() -> Result<()> {
    const RUNS: usize = 4;
    const BATCHES: usize = 4;
    const ROWS: usize = 32;

    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let mut tasks = Vec::with_capacity(RUNS);
    for run_index in 0..RUNS {
        let session = run_session(&storage, &format!("parallel-run-{run_index}"), 0);
        tasks.push(tokio::spawn(async move {
            append_batches(std::slice::from_ref(&session), 0, BATCHES, ROWS).await?;
            Ok::<_, anyhow::Error>(session)
        }));
    }

    let mut paths = Vec::with_capacity(RUNS);
    for task in tasks {
        let session = task.await??;
        assert_eq!(
            RawEventLanceStore.stats(&session).await?.row_count,
            BATCHES * ROWS
        );
        let records = replay_all_in_pages(&session, 29).await?;
        assert_eq!(records.len(), BATCHES * ROWS);
        assert_eq!(records.first().unwrap().seq, 0);
        assert_eq!(records.last().unwrap().seq, (BATCHES * ROWS - 1) as u64);
        paths.push(raw_event_lance_path(&session)?);
    }
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), RUNS, "each Run must own a separate dataset");
    Ok(())
}

#[tokio::test]
async fn run_control_takeover_fences_stale_lance_publication() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("events").to_string_lossy().into_owned();
    let control_root = temp.path().join("control").to_string_lossy().into_owned();
    let run_id = RunId::new("fenced-run");
    let session = run_session(&storage, run_id.as_str(), 0);
    let control = RunControlStore::open(&control_root).await?;

    let LeaseAcquireOutcome::Acquired(old_lease) = control
        .acquire_lease(&run_id, Some("task"), "owner-a", 60_000)
        .await?
    else {
        anyhow::bail!("owner-a did not acquire the initial lease");
    };
    let mut old_writer =
        RawEventLanceAppender::fenced(EventWriterFence::new(old_lease.epoch, "attempt-a")?);
    old_writer.activate(&session).await?;
    old_writer
        .append_event_batch(&[(session.clone(), event(0, 0))])
        .await?;

    let LeaseAcquireOutcome::Acquired(new_lease) = control
        .takeover_lease(&run_id, Some("task"), "owner-b", 60_000)
        .await?
    else {
        anyhow::bail!("owner-b did not take over the lease");
    };
    assert!(new_lease.epoch > old_lease.epoch);
    let mut new_writer =
        RawEventLanceAppender::fenced(EventWriterFence::new(new_lease.epoch, "attempt-b")?);
    // Activation is the linearization point: it fences owner-a before owner-b
    // starts writing its first data batch.
    new_writer.activate(&session).await?;

    let stale_error = old_writer
        .append_event_batch(&[(session.clone(), event(0, 1))])
        .await
        .unwrap_err();
    assert!(
        format!("{stale_error:#}").contains("stale event writer fence"),
        "{stale_error:#}"
    );
    let after_stale = replay_all_in_pages(&session, 16).await?;
    assert_eq!(after_stale.len(), 1);
    assert_eq!(after_stale[0].seq, 0);

    new_writer
        .append_event_batch(&[(session.clone(), event(0, 2))])
        .await?;
    let visible = replay_all_in_pages(&session, 16).await?;
    assert_eq!(
        visible.iter().map(|record| record.seq).collect::<Vec<_>>(),
        [0, 2]
    );
    let layout = RawEventLanceStore.layout_stats(&session).await?;
    assert_eq!(layout.active_epoch, Some(new_lease.epoch));
    assert_eq!(layout.visible_segments, 2);
    assert_eq!(layout.visible_rows, 2);
    Ok(())
}

#[tokio::test]
async fn metadata_growth_tracks_writer_epochs_not_micro_batches() -> Result<()> {
    const MICRO_BATCHES: usize = 64;
    const WRITER_EPOCHS: u64 = 4;

    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let session = run_session(&storage, "metadata-growth-run", 0);
    let mut first = RawEventLanceAppender::fenced(EventWriterFence::new(1, "writer-1")?);
    for sequence in 0..MICRO_BATCHES {
        first
            .append_event_batch(&[(session.clone(), event(0, sequence))])
            .await?;
    }
    first.finish();

    let one_epoch = RawEventLanceStore.layout_stats(&session).await?;
    assert_eq!(one_epoch.visible_segments, 1);
    assert_eq!(one_epoch.visible_fragments, MICRO_BATCHES);
    assert_eq!(one_epoch.visible_rows, MICRO_BATCHES as u64);

    for epoch in 2..=WRITER_EPOCHS {
        let mut writer =
            RawEventLanceAppender::fenced(EventWriterFence::new(epoch, format!("writer-{epoch}"))?);
        writer
            .append_event_batch(&[(session.clone(), event(0, MICRO_BATCHES + epoch as usize))])
            .await?;
    }
    let many_epochs = RawEventLanceStore.layout_stats(&session).await?;
    assert_eq!(many_epochs.visible_segments, WRITER_EPOCHS as usize);
    assert_eq!(
        many_epochs.visible_rows,
        MICRO_BATCHES as u64 + WRITER_EPOCHS - 1
    );

    RawEventLanceStore
        .maintain(
            &session,
            &LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            },
        )
        .await?;
    let compacted = RawEventLanceStore.layout_stats(&session).await?;
    assert_eq!(compacted.visible_segments, 1);
    assert_eq!(compacted.visible_fragments, 1);
    assert_eq!(compacted.visible_rows, many_epochs.visible_rows);
    Ok(())
}

#[tokio::test]
#[ignore = "opt-in sustained append stress test; tune with PCHRONICLE_STRESS_* variables"]
async fn sustained_append_stress() -> Result<()> {
    let batches = positive_env_usize("PCHRONICLE_STRESS_BATCHES", 256)?;
    let rows_per_batch = positive_env_usize("PCHRONICLE_STRESS_BATCH_ROWS", 256)?;
    let expected_rows = batches
        .checked_mul(rows_per_batch)
        .context("stress row count overflow")?;
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("store").to_string_lossy().into_owned();
    let session = run_session(&storage, "stress-run", 0);
    let sessions = vec![session.clone()];
    let started = Instant::now();

    // Periodic reopen ensures the test covers persisted state rather than only
    // one in-memory Dataset handle.
    let mut first_batch = 0;
    while first_batch < batches {
        let batch_count = (batches - first_batch).min(64);
        append_batches(&sessions, first_batch, batch_count, rows_per_batch).await?;
        first_batch += batch_count;
    }
    let elapsed = started.elapsed();

    assert_eq!(
        RawEventLanceStore.stats(&session).await?.row_count,
        expected_rows
    );
    let tail_offset = expected_rows.saturating_sub(17);
    let tail = RawEventLanceStore
        .read_events(&session, tail_offset, Some(17))
        .await?;
    assert_eq!(tail.len(), expected_rows - tail_offset);
    assert_eq!(tail.last().unwrap().seq, (expected_rows - 1) as u64);

    let rows_per_second = expected_rows as f64 / elapsed.as_secs_f64();
    eprintln!(
        "pChronicle stress: rows={expected_rows}, batches={batches}, elapsed={elapsed:?}, rows/s={rows_per_second:.0}"
    );
    Ok(())
}

fn positive_env_usize(name: &str, default: usize) -> Result<usize> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .with_context(|| format!("{name} must be a positive integer"))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error).with_context(|| format!("read {name}")),
    };
    anyhow::ensure!(value > 0, "{name} must be greater than zero");
    Ok(value)
}
