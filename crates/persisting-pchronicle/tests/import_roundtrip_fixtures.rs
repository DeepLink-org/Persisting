use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines, open_document,
};
mod support;

use support::{LookupStrategy, fixture_path, persist_and_restore};

async fn assert_openai_fixture_roundtrip(
    name: &str,
    expected_sessions: usize,
    lookups: &[LookupStrategy],
) -> Result<()> {
    let path = fixture_path(format!("import_roundtrip/{name}"));
    let stories = open_document(DocumentFormat::OpenaiMsg, &path)
        .await?
        .project_storylines()
        .await?;
    assert_eq!(stories.len(), expected_sessions);
    let expected = encode_json_storylines(DocumentFormat::OpenaiMsg, &stories)?;

    for lookup in lookups {
        let restored = persist_and_restore(&stories, *lookup).await?;

        assert_eq!(
            encode_json_storylines(DocumentFormat::OpenaiMsg, &restored)?,
            expected,
            "{name} roundtrip via {lookup:?}"
        );
    }
    Ok(())
}

async fn assert_actf_fixture_roundtrip(name: &str, lookups: &[LookupStrategy]) -> Result<()> {
    let path = fixture_path(format!("import_roundtrip/{name}"));
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read fixture {}", path.display()))?;
    let stories = decode_json_storylines(DocumentFormat::Actf, &raw, name)?;
    let expected = encode_json_storylines(DocumentFormat::Actf, &stories)?;

    for lookup in lookups {
        let restored = persist_and_restore(&stories, *lookup).await?;

        assert_eq!(
            encode_json_storylines(DocumentFormat::Actf, &restored)?,
            expected,
            "{name} roundtrip via {lookup:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cybergym_07270003_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_openai_fixture_roundtrip(
        "cybergym_07270003_trimmed.json",
        1,
        &[LookupStrategy::SessionIds],
    )
    .await
}

#[tokio::test]
async fn cybergym_0729001_multi_session_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_openai_fixture_roundtrip(
        "cybergym_0729001_trimmed.json",
        2,
        &[LookupStrategy::SessionIds, LookupStrategy::DocumentIds],
    )
    .await
}

#[tokio::test]
async fn tool_use_actf_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_actf_fixture_roundtrip(
        "make-doom-for-mips_trimmed.actf.json",
        &[LookupStrategy::SessionIds, LookupStrategy::DocumentIds],
    )
    .await
}

#[tokio::test]
async fn command_execution_actf_import_and_restore_matches_trimmed_source() -> Result<()> {
    assert_actf_fixture_roundtrip(
        "protein-assembly_trimmed.actf.json",
        &[LookupStrategy::SessionIds],
    )
    .await
}
