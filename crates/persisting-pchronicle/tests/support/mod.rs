#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::StorylineLanceStore;

pub fn fixture_path(relative: impl AsRef<Path>) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

#[derive(Clone, Copy, Debug)]
pub enum LookupStrategy {
    SessionIds,
    DocumentIds,
}

pub async fn persist_and_restore(
    stories: &[StorylineDocument],
    lookup: LookupStrategy,
) -> Result<Vec<StorylineDocument>> {
    let temporary = tempfile::tempdir()?;
    let store = StorylineLanceStore::open(temporary.path()).await?;
    store.replace_storylines(stories).await?;

    let ids = stories
        .iter()
        .map(|story| match lookup {
            LookupStrategy::SessionIds => story.session_id.clone(),
            LookupStrategy::DocumentIds => story.document_id().to_owned(),
        })
        .collect::<Vec<_>>();
    let restored = match lookup {
        LookupStrategy::SessionIds => store.get_storylines_full(&ids).await?,
        LookupStrategy::DocumentIds => store.get_storylines_by_document_ids(&ids).await?,
    };
    let lookup_name = match lookup {
        LookupStrategy::SessionIds => "session ID",
        LookupStrategy::DocumentIds => "document ID",
    };
    let mut restored = restored.into_iter();

    ids.into_iter()
        .map(|id| {
            let story = restored.next().flatten();
            story.with_context(|| {
                format!("missing Storyline for {lookup_name} `{id}` after Lance roundtrip")
            })
        })
        .collect()
}
