//! Resident sync worker snapshot import.

use super::super::*;
use super::import::{run_compact_jsonl_import, run_import};
use anyhow::{Context, Result};
use std::io::Write;

/// Run one coalesced snapshot for the resident sync worker.
///
/// - `--mirror` writes a Compact JSONL Lance Dataset (record-level ingest).
/// - `--to` writes a Storyline Lance Dataset (trajectory conversion).
///
/// Either or both destinations may be set. Each reuses the import pipeline
/// (stage progress, replace semantics, publication) so sync and import share
/// the same listing → reading → parsing → commit surface.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn sync_snapshot(
    source: &str,
    mirror: Option<&str>,
    storyline: Option<&str>,
    input_format: ExchangeFormat,
    suggested_format: Option<ExchangeFormat>,
    columns: &[String],
    stderr: &mut dyn Write,
    stderr_is_terminal: bool,
) -> Result<()> {
    anyhow::ensure!(
        mirror.is_some() || storyline.is_some(),
        "sync requires --mirror and/or --to"
    );

    if let Some(mirror) = mirror {
        let mut stdout = std::io::sink();
        run_compact_jsonl_import(
            ImportArgs {
                from: source.to_owned(),
                output: Some(mirror.to_owned()),
                format: ExchangeFormat::CompactJsonl,
                suggested_format: None,
                output_format: Some(ImportOutputFormat::CompactJsonl),
                replace: true,
                append: false,
                on_duplicate: None,
                yes: true,
                stream: false,
                max_input_bytes: Some(256 * 1024 * 1024),
                commit_every: None,
                resume: false,
                wal_dir: None,
                reset: false,
                columns: columns.to_vec(),
            },
            mirror,
            &mut stdout,
            stderr,
            stderr_is_terminal,
        )
        .await
        .context("sync source into Compact JSONL mirror")?;
    }

    if let Some(storyline) = storyline {
        anyhow::ensure!(
            input_format != ExchangeFormat::CompactJsonl,
            "sync --to requires a trajectory input format; use --mirror for compact-jsonl sources"
        );
        // ponytail: rebuild one atomic snapshot per coalesced batch; add affected-document
        // mutation when profiling shows full-directory rebuilds are the bottleneck.
        let mut stdout = std::io::sink();
        let mut stdin = std::io::empty();
        run_import(
            ImportArgs {
                from: source.to_owned(),
                output: Some(storyline.to_owned()),
                format: input_format,
                suggested_format,
                output_format: Some(ImportOutputFormat::Storyline),
                replace: true,
                append: false,
                on_duplicate: None,
                yes: true,
                stream: false,
                max_input_bytes: Some(256 * 1024 * 1024),
                commit_every: None,
                resume: false,
                wal_dir: None,
                reset: false,
                columns: Vec::new(),
            },
            None,
            false,
            stderr_is_terminal,
            &mut stdin,
            &mut stdout,
            stderr,
        )
        .await
        .context("sync source into Storyline Lance")?;
    }

    Ok(())
}
