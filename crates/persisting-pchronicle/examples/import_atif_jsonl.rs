//! Import one-trajectory-per-line ATIF into the three-table Lance store.

use std::io::{BufRead, BufReader};

use anyhow::{Context, Result};
use persisting_pchronicle::{
    into_storyline, ChronicleFormat, LanceStorylineStore, StorylineDocument,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let input = arguments
        .next()
        .context("usage: import_atif_jsonl INPUT STORE")?;
    let storage = arguments
        .next()
        .context("usage: import_atif_jsonl INPUT STORE")?;
    anyhow::ensure!(
        arguments.next().is_none(),
        "usage: import_atif_jsonl INPUT STORE"
    );

    let input = std::path::PathBuf::from(input);
    let stories = BufReader::new(
        std::fs::File::open(&input).with_context(|| format!("open {}", input.display()))?,
    )
    .lines()
    .enumerate()
    .map(|(index, line)| {
        let line = line.with_context(|| format!("read ATIF line {}", index + 1))?;
        into_storyline(ChronicleFormat::Atif, &line)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("decode ATIF line {}", index + 1))
    })
    .collect::<Result<Vec<StorylineDocument>>>()?;

    let step_count = stories.iter().map(|story| story.turns.len()).sum::<usize>();
    let store = LanceStorylineStore::open(std::path::PathBuf::from(storage)).await?;
    store.replace_storylines(&stories).await?;
    println!(
        "imported_trajectories={} imported_steps={step_count}",
        stories.len()
    );
    Ok(())
}
