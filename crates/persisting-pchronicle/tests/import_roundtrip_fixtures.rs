use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    decode_json_storylines, encode_json_storylines, open_document, DocumentFormat,
};
use persisting_pchronicle::storage::StorylineLanceStore;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/import_roundtrip")
        .join(name)
}

async fn assert_openai_fixture_roundtrip(name: &str, expected_sessions: usize) -> Result<()> {
    let path = fixture(name);
    let expected: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).with_context(|| format!("read fixture {}", path.display()))?,
    )?;
    let stories = open_document(DocumentFormat::OpenaiMsg, &path)
        .await?
        .project_storylines()
        .await?;
    assert_eq!(stories.len(), expected_sessions);

    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(&stories).await?;
    let session_ids = stories
        .iter()
        .map(|story| story.session_id.clone())
        .collect::<Vec<_>>();
    let restored = store
        .get_storylines_full(&session_ids)
        .await?
        .into_iter()
        .map(|story| story.context("missing restored OpenAI Storyline"))
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(
        encode_json_storylines(DocumentFormat::OpenaiMsg, &restored)?,
        expected
    );
    Ok(())
}

async fn assert_actf_fixture_roundtrip(name: &str) -> Result<()> {
    let path = fixture(name);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read fixture {}", path.display()))?;
    let expected: serde_json::Value = serde_json::from_str(&raw)?;
    let stories = decode_json_storylines(DocumentFormat::Actf, &raw, name)?;

    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(&stories).await?;
    let session_ids = stories
        .iter()
        .map(|story| story.session_id.clone())
        .collect::<Vec<_>>();
    let restored = store
        .get_storylines_full(&session_ids)
        .await?
        .into_iter()
        .map(|story| story.context("missing restored ACTF Storyline"))
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(
        encode_json_storylines(DocumentFormat::Actf, &restored)?,
        expected
    );
    Ok(())
}

#[tokio::test]
async fn cybergym_07270003_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_openai_fixture_roundtrip("cybergym_07270003_trimmed.json", 1).await
}

#[tokio::test]
async fn cybergym_0729001_multi_session_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_openai_fixture_roundtrip("cybergym_0729001_trimmed.json", 2).await
}

#[tokio::test]
async fn tool_use_actf_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_actf_fixture_roundtrip("make-doom-for-mips_trimmed.actf.json").await
}

#[tokio::test]
async fn command_execution_actf_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_actf_fixture_roundtrip("protein-assembly_trimmed.actf.json").await
}
