use std::path::{Path, PathBuf};

use anyhow::Result;
use datafusion::prelude::SessionContext;
use persisting_pchronicle::document::{
    encode_agenticmd, encode_json_storylines, open_document, DocumentFormat, Error, FilterPushdown,
    QueryTables, DEFAULT_DOCUMENT_MATERIALIZE_ROWS,
};
use persisting_pchronicle::model::{EventIdentity, EventRecord, StorylineDocument, StorylineTurn};
use persisting_pchronicle::storage::{RawEventLanceStore, StoryCoords, StorylineLanceStore};
use serde_json::json;

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn turn(id: i64, message: &str) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: Some("dialogue".into()),
        timestamp: None,
        source: "user".into(),
        message: json!(message),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: None,
        is_copied_context: None,
        latency_ms: None,
        ttft_ms: None,
        extra: None,
    }
}

async fn assert_storyline_tables(format: DocumentFormat, path: &Path) -> Result<()> {
    let source = open_document(format, path).await?;
    assert_eq!(source.format(), format);
    assert_eq!(
        source.register_datafusion(&SessionContext::new())?,
        QueryTables::Storyline
    );
    assert!(!source.project_storylines().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn opens_all_six_formats_and_reports_true_capabilities() -> Result<()> {
    let temporary = tempfile::tempdir()?;

    let agentic_path = temporary.path().join("story.md");
    let mut agentic_story = StorylineDocument::new("agentic-session", "agent");
    agentic_story.turns.push(turn(1, "hello"));
    std::fs::write(&agentic_path, encode_agenticmd(&agentic_story)?)?;

    let storyline_path = temporary.path().join("storyline");
    let storyline_store = StorylineLanceStore::open(&storyline_path).await?;
    storyline_store
        .replace_storylines(std::slice::from_ref(&agentic_story))
        .await?;

    let event_storage = temporary.path().join("events");
    std::fs::create_dir_all(&event_storage)?;
    let event_coords = StoryCoords::new(
        event_storage.to_string_lossy(),
        "agent",
        "event-session",
        None,
    );
    RawEventLanceStore
        .append_events(
            &event_coords,
            &[EventRecord {
                identity: EventIdentity::default(),
                seq: 1,
                source: "test".into(),
                kind: "note".into(),
                timestamp: Some("2026-01-01T00:00:00Z".into()),
                session_id: Some("event-session".into()),
                agent_id: Some("agent".into()),
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: json!({"content": "event"}),
            }],
        )
        .await?;
    let event_path = persisting_pchronicle::storage::raw_event_lance_path(&event_coords)?;

    let events = open_document(DocumentFormat::CanonicalEvent, &event_path).await?;
    assert_eq!(
        events.register_datafusion(&SessionContext::new())?,
        QueryTables::Events
    );
    let event_caps = events.capabilities();
    assert_eq!(event_caps.filter_pushdown, FilterPushdown::Exact);
    assert!(event_caps.scalar_indexes);
    assert!(event_caps.snapshot_consistent);
    assert_eq!(events.project_storylines().await?.len(), 1);

    let storyline = open_document(DocumentFormat::Storyline, &storyline_path).await?;
    assert_eq!(
        storyline.register_datafusion(&SessionContext::new())?,
        QueryTables::Storyline
    );
    let storyline_caps = storyline.capabilities();
    assert_eq!(
        storyline_caps.filter_pushdown,
        FilterPushdown::ExpressionDependent
    );
    assert!(storyline_caps.late_content_materialization);
    assert_eq!(
        storyline.project_storylines().await?,
        vec![agentic_story.clone()]
    );

    let agentic = open_document(DocumentFormat::AgenticMd, &agentic_path).await?;
    assert_eq!(
        agentic.capabilities().filter_pushdown,
        FilterPushdown::Unsupported
    );
    assert_eq!(agentic.project_storylines().await?, vec![agentic_story]);

    assert_storyline_tables(
        DocumentFormat::Atif,
        &fixture("tests/fixtures/atif/dialogue_10.json"),
    )
    .await?;
    assert_storyline_tables(
        DocumentFormat::OpenaiMsg,
        &fixture("tests/fixtures/import_roundtrip/cybergym_0729001_trimmed.json"),
    )
    .await?;
    assert_storyline_tables(
        DocumentFormat::Actf,
        &fixture("tests/fixtures/import_roundtrip/make-doom-for-mips_trimmed.actf.json"),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn materialization_budget_fails_closed_but_callback_visits_the_complete_story() -> Result<()>
{
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("large.md");
    let mut story = StorylineDocument::new("large", "agent");
    story.turns = (0..=DEFAULT_DOCUMENT_MATERIALIZE_ROWS)
        .map(|index| turn(index as i64 + 1, "x"))
        .collect();
    std::fs::write(&path, encode_agenticmd(&story)?)?;

    let source = open_document(DocumentFormat::AgenticMd, &path).await?;
    assert!(matches!(
        source.project_storylines().await,
        Err(Error::SourceBudgetExceeded { .. })
    ));
    let mut visited = Vec::new();
    source
        .for_each_storyline(|story| {
            visited.push((story.session_id, story.turns.len()));
            Ok(())
        })
        .await?;
    assert_eq!(
        visited,
        vec![("large".to_string(), DEFAULT_DOCUMENT_MATERIALIZE_ROWS + 1)]
    );
    Ok(())
}

#[tokio::test]
async fn file_document_callback_enforces_the_provider_file_budget() -> Result<()> {
    let input = tempfile::NamedTempFile::with_suffix(".json")?;
    input.as_file().set_len(1024 * 1024 * 1024)?;
    let source = open_document(DocumentFormat::OpenaiMsg, input.path()).await?;

    let error = source.for_each_storyline(|_| Ok(())).await.unwrap_err();

    assert!(error.to_string().contains("exceeding max_file_bytes"));
    Ok(())
}

#[tokio::test]
async fn atif_document_source_preserves_singleton_array_shape() -> Result<()> {
    let input = tempfile::NamedTempFile::with_suffix(".json")?;
    let expected = json!([{
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "one",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    }]);
    std::fs::write(input.path(), expected.to_string())?;

    let stories = open_document(DocumentFormat::Atif, input.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &stories)?,
        expected
    );
    Ok(())
}
