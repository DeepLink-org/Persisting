//! Opt-in S3 contract test.
//!
//! Run with:
//! `PCHRONICLE_S3_TEST_URI=s3://bucket/test-prefix cargo test -p persisting-pchronicle --test s3_storage -- --ignored`

use anyhow::{Context, Result};
use lance::io::ObjectStore;
use object_store::ObjectStoreExt;
use persisting_pchronicle::document::{decode_json_storylines, DocumentFormat};
use persisting_pchronicle::model::{EventIdentity, EventRecord, StorylineDocument};
use persisting_pchronicle::query::{ChronicleQueryEngine, ChronicleQueryExecutionOptions};
use persisting_pchronicle::storage::{
    raw_event_lance_path, LanceMaintenanceOptions, RawEventLanceAppender, RawEventLanceStore,
    StoryCoords, StorylineLanceStore,
};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

const REPLACEMENT_WORKER_ROOT_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_ROOT";
const REPLACEMENT_WORKER_SESSION_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_SESSION";
const REPLACEMENT_WORKER_BARRIER_ENV: &str = "PCHRONICLE_S3_REPLACEMENT_WORKER_BARRIER";

fn unique_root() -> Result<String> {
    let base = std::env::var("PCHRONICLE_S3_TEST_URI")
        .context("set PCHRONICLE_S3_TEST_URI to an isolated writable s3:// prefix")?;
    anyhow::ensure!(base.starts_with("s3://"), "test URI must use s3://");
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(format!(
        "{}/pchronicle-contract-{}-{suffix}",
        base.trim_end_matches('/'),
        std::process::id()
    ))
}

fn fixture_storyline() -> Result<StorylineDocument> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/atif/parallel_tools_14.json"),
    )?;
    decode_json_storylines(DocumentFormat::Atif, &source, "fixture.json")?
        .pop()
        .context("missing fixture Storyline")
}

fn fixture_storyline_with_id(session_id: &str) -> Result<StorylineDocument> {
    let mut storyline = fixture_storyline()?;
    storyline.trajectory_id = Some(session_id.to_string());
    storyline.session_id = session_id.to_string();
    storyline.run_id = Some(session_id.to_string());
    Ok(storyline)
}

async fn write_replacement_outcome(root: &str, session_id: &str, outcome: &str) -> Result<()> {
    let (store, path) = ObjectStore::from_uri(root).await?;
    store
        .inner
        .put(
            &path.join("replacement-outcomes").join(session_id),
            outcome.to_string().into(),
        )
        .await?;
    Ok(())
}

async fn read_replacement_outcome(root: &str, session_id: &str) -> Result<String> {
    let (store, path) = ObjectStore::from_uri(root).await?;
    let bytes = store
        .inner
        .get(&path.join("replacement-outcomes").join(session_id))
        .await?
        .bytes()
        .await?;
    Ok(std::str::from_utf8(&bytes)?.to_string())
}

fn event(content: &str) -> EventRecord {
    EventRecord {
        identity: EventIdentity::default(),
        seq: 0,
        source: "s3-contract".into(),
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
        payload: serde_json::json!({"content": content}),
    }
}

async fn run_contract(root: &str) -> Result<()> {
    let event_root = format!("{root}/events");
    let main = StoryCoords::new(
        &event_root,
        "contract-agent",
        "contract-run",
        Some("contract-run".into()),
    );
    let child = StoryCoords::new(
        &event_root,
        "contract-agent",
        "contract-child",
        Some("contract-run".into()),
    );
    RawEventLanceStore
        .append_events(&main, &[event("main-1")])
        .await?;
    RawEventLanceStore
        .append_events(&main, &[event("main-2")])
        .await?;
    RawEventLanceStore
        .append_events(&child, &[event("child-1")])
        .await?;
    assert_eq!(
        RawEventLanceStore.read_events(&main, 0, None).await?.len(),
        2
    );
    assert_eq!(
        RawEventLanceStore.read_events(&child, 0, None).await?.len(),
        1
    );
    assert_eq!(RawEventLanceStore.stats(&main).await?.row_count, 2);

    // Encoding fails before a write, so an invalid typed event cannot corrupt
    // the already committed S3 dataset.
    let mut invalid = event("invalid");
    invalid.seq = u64::MAX;
    assert!(RawEventLanceStore
        .append_events(&main, &[invalid])
        .await
        .is_err());
    assert_eq!(
        RawEventLanceStore.read_events(&main, 0, None).await?.len(),
        2
    );

    let storyline_root = format!("{root}/storylines");
    let store = StorylineLanceStore::open_uri(&storyline_root).await?;
    let first = fixture_storyline()?;
    store.replace_storyline(&first).await?;
    let pinned = ChronicleQueryEngine::open(
        DocumentFormat::Storyline,
        std::path::Path::new(&storyline_root),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;

    let mut second = first.clone();
    second.trajectory_id = Some("s3-contract-second".into());
    second.session_id = "s3-contract-second".into();
    second.run_id = Some("s3-contract-second".into());
    store.replace_storyline(&second).await?;

    let pinned_output = pinned
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(pinned_output.trim())?["runs"],
        1
    );
    let reopened = StorylineLanceStore::open_uri(&storyline_root).await?;
    assert_eq!(
        reopened
            .get_storylines_full(&[first.session_id.clone(), second.session_id.clone()])
            .await?,
        [Some(first), Some(second)]
    );
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Storyline,
        std::path::Path::new(&storyline_root),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let output = engine
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(output.trim())?["runs"],
        2
    );
    Ok(())
}

async fn run_append_scale_contract(root: &str) -> Result<()> {
    const BATCHES: usize = 8;
    const ROWS_PER_BATCH: usize = 32;

    let event_root = format!("{root}/event-scale");
    let session = StoryCoords::new(
        &event_root,
        "contract-agent",
        "scale-story",
        Some("scale-run".into()),
    );
    let mut writer = RawEventLanceAppender::default();
    let mut pinned = None;
    for batch_index in 0..BATCHES {
        let entries = (0..ROWS_PER_BATCH)
            .map(|row_index| {
                let sequence = batch_index * ROWS_PER_BATCH + row_index;
                let mut record = event(&format!("event-{sequence}"));
                record.seq = sequence as u64;
                (session.clone(), record)
            })
            .collect::<Vec<_>>();
        writer.append_event_batch(&entries).await?;
        if batch_index + 1 == BATCHES / 2 {
            pinned = Some(
                ChronicleQueryEngine::open(
                    DocumentFormat::CanonicalEvent,
                    raw_event_lance_path(&session)?,
                    ChronicleQueryExecutionOptions::default(),
                )
                .await?,
            );
        }
    }
    writer.finish();

    let pinned_output = pinned
        .context("pinned event query engine was not opened")?
        .query_jsonl("SELECT COUNT(*) AS rows FROM events")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(pinned_output.trim())?["rows"],
        (BATCHES * ROWS_PER_BATCH / 2) as u64
    );

    let current = ChronicleQueryEngine::open(
        DocumentFormat::CanonicalEvent,
        raw_event_lance_path(&session)?,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let current_output = current
        .query_jsonl("SELECT COUNT(*) AS rows FROM events")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(current_output.trim())?["rows"],
        (BATCHES * ROWS_PER_BATCH) as u64
    );

    let report = RawEventLanceStore
        .maintain(
            &session,
            &LanceMaintenanceOptions {
                vacuum_older_than: None,
                ..Default::default()
            },
        )
        .await?;
    assert!(report.fragments_removed >= BATCHES);
    assert_eq!(
        RawEventLanceStore.stats(&session).await?.row_count,
        BATCHES * ROWS_PER_BATCH
    );

    let mut continuation = event("after-maintenance");
    continuation.seq = (BATCHES * ROWS_PER_BATCH) as u64;
    RawEventLanceStore
        .append_events(&session, std::slice::from_ref(&continuation))
        .await?;
    let tail = RawEventLanceStore
        .read_events(&session, BATCHES * ROWS_PER_BATCH, Some(1))
        .await?;
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].seq, continuation.seq);
    assert_eq!(tail[0].payload, continuation.payload);
    assert_eq!(tail[0].identity.run_id.as_deref(), Some("scale-run"));
    assert_eq!(
        tail[0].identity.storyline_id.as_deref(),
        Some("scale-story")
    );
    Ok(())
}

async fn cleanup(root: &str) -> Result<()> {
    let (store, path) = ObjectStore::from_uri(root).await?;
    store.remove_dir_all(path).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "worker launched by s3_storyline_multiprocess_replacement_contract"]
async fn s3_storyline_replacement_worker() -> Result<()> {
    let root =
        std::env::var(REPLACEMENT_WORKER_ROOT_ENV).context("missing S3 replacement worker root")?;
    let session_id = std::env::var(REPLACEMENT_WORKER_SESSION_ENV)
        .context("missing S3 replacement worker session")?;
    let barrier = std::env::var(REPLACEMENT_WORKER_BARRIER_ENV)
        .context("missing S3 replacement worker barrier")?;
    let mut stream = std::net::TcpStream::connect(&barrier)
        .with_context(|| format!("connect replacement barrier {barrier}"))?;
    stream.write_all(&[1])?;
    let mut release = [0_u8; 1];
    stream.read_exact(&mut release)?;
    anyhow::ensure!(release == [1], "invalid replacement barrier release");

    let store = StorylineLanceStore::open_uri(&root).await?;
    let storyline = fixture_storyline_with_id(&session_id)?;
    let outcome = match store.replace_storyline(&storyline).await {
        Ok(()) => "success",
        Err(error) if error.to_string().contains("commit conflict") => "conflict",
        Err(error) => return Err(error).context("unrecognized S3 replacement failure"),
    };
    write_replacement_outcome(&root, &session_id, outcome).await
}

#[tokio::test]
#[ignore = "requires PCHRONICLE_S3_TEST_URI and writable S3 credentials"]
async fn s3_storyline_multiprocess_replacement_contract() -> Result<()> {
    let contract_root = unique_root()?;
    let storyline_root = format!("{contract_root}/storyline-replacement");
    let contract_result = async {
        let baseline = fixture_storyline_with_id("multiprocess-baseline")?;
        let seed = StorylineLanceStore::open_uri(&storyline_root).await?;
        seed.replace_storyline(&baseline).await?;

        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let barrier_address = listener.local_addr()?.to_string();
        let executable = std::env::current_exe()?;
        let sessions = ["multiprocess-left", "multiprocess-right"];
        let mut children = sessions
            .iter()
            .map(|session_id| {
                Command::new(&executable)
                    .args([
                        "--exact",
                        "s3_storyline_replacement_worker",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env(REPLACEMENT_WORKER_ROOT_ENV, &storyline_root)
                    .env(REPLACEMENT_WORKER_SESSION_ENV, session_id)
                    .env(REPLACEMENT_WORKER_BARRIER_ENV, &barrier_address)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("spawn replacement worker {session_id}"))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut waiters = Vec::with_capacity(sessions.len());
        for _ in &sessions {
            let (mut stream, _) = listener.accept()?;
            let mut ready = [0_u8; 1];
            stream.read_exact(&mut ready)?;
            anyhow::ensure!(ready == [1], "invalid replacement worker readiness");
            waiters.push(stream);
        }
        for stream in &mut waiters {
            stream.write_all(&[1])?;
        }
        drop(waiters);

        for (session_id, child) in sessions.iter().zip(children.drain(..)) {
            let output = child.wait_with_output()?;
            anyhow::ensure!(
                output.status.success(),
                "replacement worker {session_id} failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let mut successful_sessions = Vec::new();
        for session_id in sessions {
            match read_replacement_outcome(&storyline_root, session_id)
                .await?
                .as_str()
            {
                "success" => successful_sessions.push(session_id.to_string()),
                "conflict" => {}
                outcome => anyhow::bail!(
                    "replacement worker {session_id} reported unrecognized outcome {outcome}"
                ),
            }
        }
        anyhow::ensure!(
            !successful_sessions.is_empty(),
            "all S3 replacement workers conflicted"
        );

        let reopened = StorylineLanceStore::open_uri(&storyline_root).await?;
        anyhow::ensure!(
            reopened
                .get_storyline_full(&baseline.session_id)
                .await?
                .as_ref()
                == Some(&baseline),
            "baseline Storyline was lost"
        );
        for session_id in successful_sessions {
            anyhow::ensure!(
                reopened.get_storyline_full(&session_id).await?.is_some(),
                "successful replacement {session_id} is absent from CURRENT"
            );
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let cleanup_result = cleanup(&contract_root).await;
    match (contract_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => {
            Err(error).context("S3 replacement contract passed but cleanup failed")
        }
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "S3 replacement contract cleanup also failed: {cleanup_error:#}"
        )),
    }
}

#[tokio::test]
#[ignore = "requires PCHRONICLE_S3_TEST_URI and writable S3 credentials"]
async fn s3_event_storyline_and_query_contract() -> Result<()> {
    let root = unique_root()?;
    let contract_result = run_contract(&root).await;
    let cleanup_result = cleanup(&root).await;
    match (contract_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("S3 contract passed but cleanup failed"),
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "S3 contract cleanup also failed: {cleanup_error:#}"
        )),
    }
}

#[tokio::test]
#[ignore = "requires PCHRONICLE_S3_TEST_URI and writable S3 credentials"]
async fn s3_append_scale_snapshot_and_maintenance_contract() -> Result<()> {
    let root = unique_root()?;
    let contract_result = run_append_scale_contract(&root).await;
    let cleanup_result = cleanup(&root).await;
    match (contract_result, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error).context("S3 scale contract passed but cleanup failed"),
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "S3 scale contract cleanup also failed: {cleanup_error:#}"
        )),
    }
}
