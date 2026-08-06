//! Opt-in S3 contract test.
//!
//! Run with:
//! `PCHRONICLE_S3_TEST_URI=s3://bucket/test-prefix cargo test -p persisting-pchronicle --test s3_storage -- --ignored`

use anyhow::{Context, Result};
use lance::io::ObjectStore;
use persisting_pchronicle::{
    into_storyline, AtifTrajectory, ChronicleFormat, ChronicleQueryEngine, EventRecord,
    RawEventLanceStore, StoryCoords, StorylineLanceStore, StructuredStore,
};

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

fn fixture_storyline() -> Result<persisting_pchronicle::StorylineDocument> {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/atif/parallel_tools_14.json"),
    )?;
    let trajectory = AtifTrajectory::from_json_str(&source)?;
    into_storyline(ChronicleFormat::Atif, &serde_json::to_string(&trajectory)?).map_err(Into::into)
}

fn event(content: &str) -> EventRecord {
    EventRecord {
        identity: persisting_pchronicle::EventIdentity::default(),
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

    // Decoding fails before a write, so an invalid append cannot corrupt the
    // already committed S3 dataset.
    assert!(RawEventLanceStore
        .append(&main, &["invalid RON".into()])
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
    let pinned = ChronicleQueryEngine::open_lance_uri(&storyline_root).await?;

    let mut second = first.clone();
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
    assert_eq!(reopened.list_runs().await?.len(), 2);
    let engine = ChronicleQueryEngine::open_lance_uri(&storyline_root).await?;
    let output = engine
        .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
        .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(output.trim())?["runs"],
        2
    );
    Ok(())
}

async fn cleanup(root: &str) -> Result<()> {
    let (store, path) = ObjectStore::from_uri(root).await?;
    store.remove_dir_all(path).await?;
    Ok(())
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
