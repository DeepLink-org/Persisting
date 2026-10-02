use std::path::Path;

use anyhow::Result;
use datafusion::prelude::SessionContext;
use persisting_pchronicle::document::{
    DEFAULT_DOCUMENT_MATERIALIZE_ROWS, DocumentFormat, FilterPushdown, QueryTables,
    encode_agenticmd, encode_json_storylines, open_document,
};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use persisting_pchronicle::storage::StorylineLanceStore;
use serde_json::json;

mod support;

use support::fixture_path;

fn accepts_anyhow<T>(result: anyhow::Result<T>) -> anyhow::Result<T> {
    result
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
        env: None,
        prompt: None,
        finished_at: None,
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
async fn opens_supported_formats_and_reports_true_capabilities() -> Result<()> {
    let temporary = tempfile::tempdir()?;

    let agentic_path = temporary.path().join("story.md");
    let mut agentic_story = StorylineDocument::new("agentic-session", "agent");
    agentic_story.turns.push(turn(1, "hello"));
    std::fs::write(&agentic_path, encode_agenticmd(&agentic_story)?)?;

    let storyline_json_path = temporary.path().join("story.storyline.json");
    std::fs::write(
        &storyline_json_path,
        serde_json::to_vec_pretty(&encode_json_storylines(
            DocumentFormat::Storyline,
            std::slice::from_ref(&agentic_story),
        )?)?,
    )?;

    let storyline_path = temporary.path().join("storyline");
    let storyline_store = StorylineLanceStore::open(&storyline_path).await?;
    storyline_store
        .replace_storylines(std::slice::from_ref(&agentic_story))
        .await?;

    let storyline = open_document(DocumentFormat::StorylineLance, &storyline_path).await?;
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

    assert_storyline_tables(DocumentFormat::Storyline, &storyline_json_path).await?;

    assert_storyline_tables(DocumentFormat::Atif, &fixture_path("atif/dialogue_10.json")).await?;
    assert_storyline_tables(
        DocumentFormat::OpenaiMsg,
        &fixture_path("import_roundtrip/cybergym_0729001_trimmed.json"),
    )
    .await?;
    let actf = open_document(
        DocumentFormat::Actf,
        &fixture_path("import_roundtrip/make-doom-for-mips_trimmed.actf.json"),
    )
    .await?;
    assert_eq!(
        actf.register_datafusion(&SessionContext::new())?,
        QueryTables::Storyline
    );
    let actf_caps = actf.capabilities();
    assert_eq!(actf_caps.filter_pushdown, FilterPushdown::Inexact);
    assert!(actf_caps.streaming_decode);
    assert!(!actf.project_storylines().await?.is_empty());
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
    let error = accepts_anyhow(source.project_storylines().await).unwrap_err();
    assert!(format!("{error:#}").contains("materialized rows"));
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
async fn file_document_callbacks_enforce_the_provider_file_budget() -> Result<()> {
    let input = tempfile::NamedTempFile::with_suffix(".json")?;
    input.as_file().set_len(1024 * 1024 * 1024)?;
    for format in [
        DocumentFormat::Atif,
        DocumentFormat::Actf,
        DocumentFormat::OpenaiMsg,
    ] {
        let source = open_document(format, input.path()).await?;
        let error = source.for_each_storyline(|_| Ok(())).await.unwrap_err();
        assert!(
            error.to_string().contains("exceeding max_file_bytes"),
            "{format}: {error:#}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn atif_document_source_canonicalizes_singleton_array() -> Result<()> {
    let input = tempfile::NamedTempFile::with_suffix(".json")?;
    let trajectory = json!({
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "one",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    });
    std::fs::write(input.path(), json!([trajectory]).to_string())?;

    let stories = open_document(DocumentFormat::Atif, input.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &stories)?,
        json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "one",
            "session_id": "one",
            "agent": {"name": "agent", "version": "1"},
            "steps": []
        })
    );
    Ok(())
}
