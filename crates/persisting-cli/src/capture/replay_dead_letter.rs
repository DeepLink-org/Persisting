//! `persisting capture replay-dead-letter` — re-apply failed capture events.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use persisting_capture::{replay_dead_letter, CaptureEngine, CaptureSink};
use persisting_capture::session_index::SessionIndexStore;

use super::CaptureFormat;

pub struct ReplayDeadLetterOptions {
    pub output_dir: PathBuf,
    pub format: CaptureFormat,
    pub sink: Arc<dyn CaptureSink>,
}

pub fn cmd_replay_dead_letter(opts: ReplayDeadLetterOptions) -> Result<()> {
    let storage = opts
        .output_dir
        .canonicalize()
        .unwrap_or(opts.output_dir);
    let index_store = SessionIndexStore::open(storage.as_path())?;
    let index = index_store.clone_handle();
    let stream_markdown = opts.format.stream_markdown_in_engine();

    let rt = tokio::runtime::Runtime::new().context("tokio runtime")?;
    let summary = rt.block_on(async {
        let engine = CaptureEngine::new(
            opts.sink,
            index,
            Arc::new(storage.clone()),
            stream_markdown,
        )
        .await
        .context("capture engine")?;
        replay_dead_letter(storage.as_path(), &engine).await
    })?;

    eprintln!(
        "[persisting-cli] replay dead letter: attempted={} succeeded={} failed={}",
        summary.attempted, summary.succeeded, summary.failed
    );
    Ok(())
}
