use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    actf_to_storylines, recover_openai_msg_files, storylines_to_actf,
};
use persisting_pchronicle::document::{atif_to_storyline, storyline_to_atif};
use persisting_pchronicle::model::{
    ActfDocument, AtifTrajectory, OpenaiMsgCorpusReader, StorylineDocument,
};
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
    let session_ids = stories
        .iter()
        .map(|story| story.session_id.clone())
        .collect::<Vec<_>>();
    store
        .get_storylines_full(&session_ids)
        .await?
        .into_iter()
        .map(|story| story.context("Storyline Lance roundtrip lost a session"))
        .collect()
}

#[tokio::test]
async fn atif_actf_and_openai_are_lossless_through_storyline_lance() -> Result<()> {
    let atif_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atif/parallel_tools_14.json");
    let atif_raw = std::fs::read_to_string(&atif_path)?;
    let atif_expected: serde_json::Value = serde_json::from_str(&atif_raw)?;
    let atif = AtifTrajectory::from_json_str(&atif_raw)?;
    let atif_restored = persist_and_restore(&[atif_to_storyline(&atif)?]).await?;
    assert_eq!(
        serde_json::to_value(storyline_to_atif(&atif_restored[0])?)?,
        atif_expected
    );

    let actf_path = fixture("make-doom-for-mips_trimmed.actf.json");
    let actf_raw = std::fs::read_to_string(&actf_path)?;
    let actf_expected: serde_json::Value = serde_json::from_str(&actf_raw)?;
    let actf = ActfDocument::from_json_str(&actf_raw)?;
    let actf_restored = persist_and_restore(&actf_to_storylines(&actf)?).await?;
    assert_eq!(
        serde_json::to_value(storylines_to_actf(&actf_restored)?)?,
        actf_expected
    );

    let openai_path = fixture("cybergym_0729001_trimmed.json");
    let openai_expected: serde_json::Value = serde_json::from_slice(&std::fs::read(&openai_path)?)?;
    let openai_stories = OpenaiMsgCorpusReader::open(&openai_path)?
        .collect::<persisting_pchronicle::document::Result<Vec<_>>>()?;
    let openai_restored = persist_and_restore(&openai_stories).await?;
    let recovered = recover_openai_msg_files(&openai_restored)?;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].document, openai_expected);
    Ok(())
}
