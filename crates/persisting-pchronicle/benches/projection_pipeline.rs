//! Canonical event append and events → Storyline materialization benchmark.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, storyline_to_events,
};
use persisting_pchronicle::model::{EventIdentity, EventRecord, StorylineDocument};
use persisting_pchronicle::storage::{
    RawEventLanceAppender, StoryCoords, StorylineProjectionBuildOutcome,
    StorylineProjectionSyncOutcome, build_storyline_projection, sync_storyline_projection,
    verify_storyline_projection,
};

fn main() -> Result<()> {
    let scale = env_usize("PCHRONICLE_BENCH_SCALE", 32);
    anyhow::ensure!(scale > 0, "PCHRONICLE_BENCH_SCALE must be positive");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(scale))?;

    println!(
        "canonical projection: {} Storylines, {} initial events",
        result.storylines, result.events
    );
    println!(
        "RESULT benchmark=event_append rows={} initial_append_ms={:.3} rows_s={:.1}",
        result.events,
        milliseconds(result.initial_append),
        result.events as f64 / result.initial_append.as_secs_f64(),
    );
    println!(
        "RESULT benchmark=projection_build documents={} build_ms={:.3} storylines_s={:.1}",
        result.storylines,
        milliseconds(result.projection_build),
        result.storylines as f64 / result.projection_build.as_secs_f64(),
    );
    println!(
        "RESULT benchmark=projection_incremental suffix_append_ms={:.3} sync_ms={:.3}",
        milliseconds(result.suffix_append),
        milliseconds(result.projection_sync),
    );
    println!(
        "RESULT benchmark=projection_verify verify_ms={:.3}",
        milliseconds(result.projection_verify),
    );
    Ok(())
}

struct BenchmarkResult {
    storylines: usize,
    events: usize,
    initial_append: Duration,
    projection_build: Duration,
    suffix_append: Duration,
    projection_sync: Duration,
    projection_verify: Duration,
}

async fn run(scale: usize) -> Result<BenchmarkResult> {
    let temporary = tempfile::tempdir()?;
    let storage = temporary.path().join("canonical");
    let output = temporary.path().join("storyline");
    let stories = expand_stories(&load_base_stories()?, scale);
    let root_session_id = "benchmark-run".to_string();
    let mut entries = Vec::new();
    for story in &stories {
        let coords = StoryCoords::new(
            storage.to_string_lossy(),
            "benchmark-agent",
            story.session_id.clone(),
            Some(root_session_id.clone()),
        );
        let events = storyline_to_events(story)?;
        entries.extend(
            events
                .events
                .into_iter()
                .map(|event| (coords.clone(), event)),
        );
    }
    let events = entries.len();
    let source_uri = entries
        .first()
        .context("projection benchmark corpus is empty")?
        .0
        .lance_event_path()?
        .to_string_lossy()
        .into_owned();
    let output_uri = output.to_string_lossy().into_owned();

    let mut appender = RawEventLanceAppender::default();
    let started = Instant::now();
    let appended = appender.append_event_batch(&entries).await?;
    let initial_append = started.elapsed();
    anyhow::ensure!(appended.accepted_records == events);

    let started = Instant::now();
    let StorylineProjectionBuildOutcome::Built(build) =
        build_storyline_projection(&source_uri, &output_uri, "events.lance").await?
    else {
        anyhow::bail!("benchmark projection output was unexpectedly nonempty")
    };
    let projection_build = started.elapsed();
    anyhow::ensure!(build.storylines == stories.len());

    let affected = &entries[0].0;
    let suffix = EventRecord {
        identity: EventIdentity {
            event_id: Some("benchmark-suffix".into()),
            ..Default::default()
        },
        seq: 1_000_000,
        source: "benchmark".into(),
        kind: "note".into(),
        timestamp: None,
        session_id: Some(affected.session_id.clone()),
        agent_id: Some(affected.agent_id.clone()),
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: serde_json::json!({"content": "incremental projection benchmark"}),
    };
    let started = Instant::now();
    appender
        .append_event_batch(&[(affected.clone(), suffix)])
        .await?;
    let suffix_append = started.elapsed();

    let started = Instant::now();
    let StorylineProjectionSyncOutcome::Synced(sync) =
        sync_storyline_projection(&source_uri, &output_uri).await?
    else {
        anyhow::bail!("benchmark projection unexpectedly required a rebuild")
    };
    let projection_sync = started.elapsed();
    anyhow::ensure!(sync.affected_storylines == 1 && sync.suffix_rows_scanned == 1);

    let started = Instant::now();
    let verification = verify_storyline_projection(&source_uri, &output_uri).await?;
    let projection_verify = started.elapsed();
    anyhow::ensure!(verification.fresh);

    Ok(BenchmarkResult {
        storylines: stories.len(),
        events,
        initial_append,
        projection_build,
        suffix_append,
        projection_sync,
        projection_verify,
    })
}

fn load_base_stories() -> Result<Vec<StorylineDocument>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif");
    let mut paths = std::fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| path.extension().and_then(|value| value.to_str()) == Some("json"));
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let raw = std::fs::read_to_string(&path)?;
            decode_json_storylines(DocumentFormat::Atif, &raw, &path)?
                .pop()
                .context("missing benchmark Storyline")
        })
        .collect()
}

fn expand_stories(base: &[StorylineDocument], scale: usize) -> Vec<StorylineDocument> {
    let mut stories = Vec::with_capacity(base.len() * scale);
    for replica in 0..scale {
        for (index, source) in base.iter().enumerate() {
            let mut story = source.clone();
            story.session_id = format!("projection-{replica:06}-{index:02}");
            story.run_id = Some("benchmark-run".into());
            stories.push(story);
        }
    }
    stories
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
