use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    decode_json_storylines, encode_json_storylines, open_document, DocumentFormat,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::StorylineLanceStore;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/import_roundtrip")
        .join(name)
}

async fn persist_and_restore(stories: &[StorylineDocument]) -> Result<Vec<StorylineDocument>> {
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(stories).await?;
    let document_ids = stories
        .iter()
        .map(|story| {
            story
                .trajectory_id
                .clone()
                .unwrap_or_else(|| story.session_id.clone())
        })
        .collect::<Vec<_>>();
    store
        .get_storylines_by_document_ids(&document_ids)
        .await?
        .into_iter()
        .map(|story| story.context("Storyline Lance roundtrip lost a session"))
        .collect()
}

#[tokio::test]
async fn nested_atif_and_null_presence_are_lossless_through_storyline_lance() -> Result<()> {
    let expected = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "session_id": "shared-run",
        "trajectory_id": "root",
        "agent": {"name": "root", "version": "1", "model_name": null},
        "steps": [{
            "step_id": 1,
            "timestamp": "2026-08-14T12:34:56.789123+08:00",
            "source": "agent",
            "message": "root",
            "llm_call_count": null
        }],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "child",
            "agent": {"name": "child", "version": "1"},
            "steps": [],
            "notes": null
        }]
    });
    let stories =
        decode_json_storylines(DocumentFormat::Atif, &expected.to_string(), "nested.json")?;
    let restored = persist_and_restore(&stories).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn atif_actf_and_openai_are_lossless_through_storyline_lance() -> Result<()> {
    let atif_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif/parallel_tools_14.json");
    let atif_raw = std::fs::read_to_string(&atif_path)?;
    let atif_expected: serde_json::Value = serde_json::from_str(&atif_raw)?;
    let atif_stories = decode_json_storylines(DocumentFormat::Atif, &atif_raw, &atif_path)?;
    let atif_restored = persist_and_restore(&atif_stories).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &atif_restored)?,
        atif_expected
    );

    let actf_path = fixture("make-doom-for-mips_trimmed.actf.json");
    let actf_raw = std::fs::read_to_string(&actf_path)?;
    let actf_expected: serde_json::Value = serde_json::from_str(&actf_raw)?;
    let actf_stories = decode_json_storylines(DocumentFormat::Actf, &actf_raw, &actf_path)?;
    let actf_restored = persist_and_restore(&actf_stories).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Actf, &actf_restored)?,
        actf_expected
    );

    let openai_path = fixture("cybergym_0729001_trimmed.json");
    let openai_expected: serde_json::Value = serde_json::from_slice(&std::fs::read(&openai_path)?)?;
    let openai_stories = open_document(DocumentFormat::OpenaiMsg, &openai_path)
        .await?
        .project_storylines()
        .await?;
    let openai_restored = persist_and_restore(&openai_stories).await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::OpenaiMsg, &openai_restored)?,
        openai_expected
    );
    Ok(())
}

#[tokio::test]
async fn atif_root_order_is_lossless_through_the_unified_storyline_source() -> Result<()> {
    let expected = serde_json::json!([
        {
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "z-document",
            "session_id": "shared",
            "agent": {"name": "z", "version": "1"},
            "steps": []
        },
        {
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "a-document",
            "session_id": "shared",
            "agent": {"name": "a", "version": "1"},
            "steps": []
        }
    ]);
    let decoded =
        decode_json_storylines(DocumentFormat::Atif, &expected.to_string(), "ordered.json")?;
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(&decoded).await?;

    let restored = open_document(DocumentFormat::Storyline, temporary.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn atif_singleton_array_shape_is_lossless_through_storyline_lance() -> Result<()> {
    let expected = serde_json::json!([{
        "schema_version": "ATIF-v1.7",
        "trajectory_id": "only-document",
        "agent": {"name": "agent", "version": "1"},
        "steps": []
    }]);
    let temporary = tempfile::tempdir()?;
    let input = temporary.path().join("singleton.json");
    std::fs::write(&input, expected.to_string())?;
    let lance = temporary.path().join("storyline");
    let store = StorylineLanceStore::open(&lance).await?;
    store.import_atif_stream(&input).await?;

    let restored = open_document(DocumentFormat::Storyline, &lance)
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        encode_json_storylines(DocumentFormat::Atif, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn incremental_storyline_replace_preserves_global_collection_order() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    let mut z = StorylineDocument::new("z-document", "agent");
    z.trajectory_id = Some("z-document".into());
    let mut a = StorylineDocument::new("a-document", "agent");
    a.trajectory_id = Some("a-document".into());
    store.replace_storyline(&z).await?;
    store.replace_storyline(&a).await?;

    let source = open_document(DocumentFormat::Storyline, temporary.path()).await?;
    let first = source.project_storylines().await?;
    assert_eq!(
        first
            .iter()
            .map(StorylineDocument::document_id)
            .collect::<Vec<_>>(),
        ["z-document", "a-document"]
    );

    z.notes = Some("updated".into());
    store.replace_storyline(&z).await?;
    let second = open_document(DocumentFormat::Storyline, temporary.path())
        .await?
        .project_storylines()
        .await?;
    assert_eq!(
        second
            .iter()
            .map(StorylineDocument::document_id)
            .collect::<Vec<_>>(),
        ["z-document", "a-document"]
    );
    assert_eq!(second[0].notes.as_deref(), Some("updated"));
    Ok(())
}
