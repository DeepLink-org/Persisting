//! Import command: storyline squash/commit/finalize, compact, and event paths.

use super::super::*;
use super::decode::*;
use super::drop::confirm_destructive_dataset;
use super::pipeline::*;
use super::progress::{CliProgress, StageHandle, StageId, format_byte_count};
use super::staging::*;
use anyhow::{Context, Result, anyhow};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::StorylineLanceStore;
use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

pub(crate) async fn prepare_import_destination(
    args: &ImportArgs,
    output_arg: &str,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
) -> Result<PreparedImportDestination> {
    let parsed = DatasetLocation::parse(output_arg)?;
    let exists = parsed.exists().await?;
    match args.mode()? {
        ImportMode::Create => {
            if parsed.is_object_store() {
                anyhow::ensure!(!exists, "import output already exists");
                Ok(PreparedImportDestination {
                    location: parsed,
                    replace_existing: false,
                })
            } else {
                Ok(PreparedImportDestination {
                    location: parsed.into_create_target()?,
                    replace_existing: false,
                })
            }
        }
        ImportMode::Append => {
            if !exists {
                return Err(cli_boundary_error(
                    BoundaryCode::NotFound,
                    format!("append target Dataset does not exist: {}", parsed.as_str()),
                ));
            }
            let location = if parsed.local_path().is_some() {
                parsed.into_existing()?
            } else {
                parsed
            };
            Ok(PreparedImportDestination {
                location,
                replace_existing: false,
            })
        }
        ImportMode::Replace => {
            if !exists {
                return if parsed.is_object_store() {
                    Ok(PreparedImportDestination {
                        location: parsed,
                        replace_existing: false,
                    })
                } else {
                    Ok(PreparedImportDestination {
                        location: parsed.into_create_target()?,
                        replace_existing: false,
                    })
                };
            }
            let existing = parsed.into_existing()?;
            ensure_import_source_outside_destination(args, &existing)?;
            confirm_destructive_dataset(
                "replace",
                existing.as_str(),
                args.yes,
                stdin_is_terminal,
                stdin,
                stderr,
            )?;
            Ok(PreparedImportDestination {
                location: existing,
                replace_existing: true,
            })
        }
    }
}

pub(crate) struct PreparedImportDestination {
    location: DatasetLocation,
    replace_existing: bool,
}

pub(crate) fn ensure_import_source_outside_destination(
    args: &ImportArgs,
    destination: &DatasetLocation,
) -> Result<()> {
    let (Some(source), Some(target)) = (
        (args.from != "-").then(|| Path::new(&args.from)),
        destination.local_path(),
    ) else {
        return Ok(());
    };
    let source = std::fs::canonicalize(source).context("canonicalize replace import source")?;
    anyhow::ensure!(
        !source.starts_with(target),
        "replace import source is inside the Dataset that would be replaced"
    );
    Ok(())
}

pub(crate) async fn run_import(
    mut args: ImportArgs,
    settings_override: Option<&Path>,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    args.stream = args.from == "-" || args.stream;
    reset_import_log()?;
    let max_input_bytes = match args.max_input_bytes {
        Some(0) => {
            return Err(anyhow!("--max-input-bytes must be greater than zero"));
        }
        Some(limit) => limit,
        None => usize::MAX,
    };
    anyhow::ensure!(
        args.from == "-" || !args.stream,
        "--stream requires --from -"
    );
    if args.stream {
        anyhow::ensure!(
            args.format != ExchangeFormat::Auto,
            "stdin import requires an explicit --input-format"
        );
    }
    if let Some(suggested) = args.suggested_format {
        anyhow::ensure!(
            args.format == ExchangeFormat::Auto,
            "--suggested-format is only valid with --format auto"
        );
        anyhow::ensure!(
            suggested != ExchangeFormat::Auto,
            "--suggested-format cannot be auto"
        );
        anyhow::ensure!(
            suggested != ExchangeFormat::CompactJsonl,
            "--suggested-format cannot be compact-jsonl; pass --format compact-jsonl instead"
        );
    }
    let mode = args.mode()?;
    anyhow::ensure!(
        mode == ImportMode::Append || args.on_duplicate.is_none(),
        "--on-duplicate is only valid with --append"
    );
    anyhow::ensure!(
        mode == ImportMode::Replace || !args.yes,
        "--yes is only valid with --replace"
    );
    anyhow::ensure!(
        !(args.stream && mode == ImportMode::Replace && !args.yes),
        "stdin replace import requires --yes because stdin carries the import data"
    );
    if args.from != "-" {
        args.from = expand_dataset_reference(&args.from, settings_override, true)?;
    }
    let from_location = (!args.stream)
        .then(|| DatasetLocation::parse(&args.from))
        .transpose()?;
    let canonical = if let Some(location) = &from_location {
        let looks_like_store = location.is_object_store()
            || location.local_path().is_some_and(std::path::Path::is_dir);
        if looks_like_store {
            probe_canonical_event_store(location.as_str()).await?
        } else {
            None
        }
    } else {
        None
    };
    let output_arg = match args.output.as_deref() {
        Some(output) => expand_dataset_reference(output, settings_override, false)?,
        None => default_import_output(&args, settings_override)?,
    };
    if args.format == ExchangeFormat::CompactJsonl
        || args.output_format == Some(ImportOutputFormat::CompactJsonl)
    {
        args.format = ExchangeFormat::CompactJsonl;
        return run_compact_jsonl_import(args, &output_arg, stdout, stderr, stderr_is_terminal)
            .await;
    }
    let requested_destination = DatasetLocation::parse(&output_arg)?;
    if canonical.is_none()
        && requested_destination.is_object_store()
        && args.output_format != Some(ImportOutputFormat::Storyline)
    {
        anyhow::ensure!(
            mode == ImportMode::Append && args.output_format.is_none(),
            "object-store import requires --output-format storyline"
        );
    }
    let prepared =
        prepare_import_destination(&args, &output_arg, stdin_is_terminal, stdin, stderr).await?;
    let destination = prepared.location;
    let replace_existing = prepared.replace_existing;
    if let Some(snapshot) = canonical {
        anyhow::ensure!(
            mode != ImportMode::Append,
            "canonical event import does not support --append"
        );
        return run_canonical_event_import(
            args,
            snapshot,
            destination,
            replace_existing,
            stdout,
            stderr,
        )
        .await;
    }
    let mut progress = CliProgress::new(stderr_is_terminal);
    let _s3_throttle_ui = progress.attach_object_store_throttle();
    let object_store_from = from_location
        .as_ref()
        .filter(|location| location.is_object_store() && !args.stream)
        .cloned();
    let (directory_input, candidates) = if args.stream {
        progress.set_discovered(1, 0)?;
        (false, Vec::new())
    } else if object_store_from.is_some() {
        // Object-store Sources are discovered inside the Storyline pipeline so
        // listing overlaps read/parse/write instead of buffering the full tree.
        (true, Vec::new())
    } else if from_location.is_some() {
        let (directory_input, candidates) = collect_import_candidates(Path::new(&args.from))?;
        let discovered_bytes = candidates.iter().try_fold(0u64, |total, candidate| {
            total
                .checked_add(candidate.size_hint)
                .context("import discovered byte count overflow")
        })?;
        progress.set_discovered(candidates.len() as u64, discovered_bytes)?;
        (directory_input, candidates)
    } else {
        (false, Vec::new())
    };
    anyhow::ensure!(
        mode != ImportMode::Append || args.output_format != Some(ImportOutputFormat::Preserve),
        "append import requires --output-format storyline (or omit it)"
    );
    let output_format = args.output_format.unwrap_or(if mode == ImportMode::Append {
        ImportOutputFormat::Storyline
    } else {
        ImportOutputFormat::Preserve
    });
    let duplicate_policy = args.on_duplicate.unwrap_or(DuplicateIdPolicy::Suffix);
    let (wal, skip_paths) =
        open_import_wal(&args, &args.from, destination.as_str(), output_format)?;
    if let Some(wal) = &wal
        && let Ok(guard) = wal.lock()
    {
        progress.notice(&format!(
            "import_wal={} job_id={} done={} failed={} resume={}",
            guard.dir().display(),
            guard.job().job_id,
            guard.done_count(),
            guard.failed_count(),
            args.resume,
        ))?;
    }
    let (dataset_uri, imported_sources, unknown_field_warnings, skipped_warnings) = if mode
        == ImportMode::Append
    {
        let store = StorylineLanceStore::open_uri(destination.as_str())
            .await
            .context("open append target as a Storyline Lance Dataset")?;
        anyhow::ensure!(
            store.current_table_paths().await?.is_some(),
            "append target is not a committed Storyline Dataset"
        );
        let (append_generation, existing_document_ids) = store
            .document_ids_snapshot()
            .await?
            .context("append target has no committed Storyline snapshot")?;
        let existing_storyline_count = existing_document_ids.len() as u64;
        let existing_document_ids = existing_document_ids.into_iter().collect();
        let (imported_sources, unknown_field_warnings, skipped_warnings) =
            squash_storyline_into_store(
                &store,
                &args,
                stdin,
                &mut progress,
                &candidates,
                object_store_from.clone(),
                StorylineImportOptions {
                    max_input_bytes,
                    directory_input,
                    seen_document_ids: existing_document_ids,
                    duplicate_policy,
                    allow_empty: true,
                    append_generation: Some(append_generation),
                    initial_storyline_count: existing_storyline_count,
                    wal: wal.clone(),
                    skip_paths: Arc::clone(&skip_paths),
                },
            )
            .await?;
        (
            destination.as_str().to_string(),
            imported_sources,
            unknown_field_warnings,
            skipped_warnings,
        )
    } else if destination.is_object_store() || output_format == ImportOutputFormat::Storyline {
        // Storyline imports commit in place so progressive CURRENT +
        // chronicle.manifest updates are visible to a live catalog mount.
        if destination.exists().await? && !replace_existing {
            return Err(cli_boundary_error(
                BoundaryCode::Conflict,
                "import output already exists",
            ));
        }
        let (imported_sources, unknown_field_warnings, skipped_warnings) = if destination
            .is_object_store()
        {
            // Write directly to the remote Dataset. Progressive commits must be
            // visible on the destination during long imports; local staging +
            // final upload hides all progress until the job finishes.
            if replace_existing {
                destination
                    .remove_all_with_progress(|deleted, total, path| {
                        progress.note_deleted(deleted, total, path)
                    })
                    .await
                    .with_context(|| {
                        format!("delete replaced Dataset prefix {}", destination.as_str())
                    })?;
            }
            let store = StorylineLanceStore::open_uri(destination.as_str())
                .await
                .with_context(|| {
                    format!(
                        "open remote Storyline Dataset for import at {}",
                        destination.as_str()
                    )
                })?;
            squash_storyline_into_store(
                &store,
                &args,
                stdin,
                &mut progress,
                &candidates,
                object_store_from.clone(),
                StorylineImportOptions::create(max_input_bytes, directory_input)
                    .with_wal(wal.clone(), Arc::clone(&skip_paths)),
            )
            .await?
        } else {
            let output = destination
                .local_path()
                .context("local Storyline output must be a filesystem path")?;
            let staging = tempfile::Builder::new()
                .prefix(".pchronicle-storyline-stage-")
                .tempdir_in(output.parent().context("Storyline output has no parent")?)
                .context("create local Storyline staging directory")?;
            let store = StorylineLanceStore::open(staging.path())
                .await
                .context("create staged Storyline Lance Dataset")?;
            let result = squash_storyline_into_store(
                &store,
                &args,
                stdin,
                &mut progress,
                &candidates,
                object_store_from.clone(),
                StorylineImportOptions::create(max_input_bytes, directory_input)
                    .with_wal(wal.clone(), Arc::clone(&skip_paths)),
            )
            .await?;
            let staging_path = staging.keep();
            let mut cleanup = StagingPathGuard::new(staging_path.clone());
            publish_staged_dataset(&staging_path, output, replace_existing, Some(&mut progress))
                .await?;
            cleanup.disarm();
            result
        };
        (
            destination.as_str().to_string(),
            imported_sources,
            unknown_field_warnings,
            skipped_warnings,
        )
    } else {
        let output = destination
            .local_path()
            .context("local import output must be a filesystem path")?
            .to_path_buf();
        let parent = output
            .parent()
            .context("import output must have a parent directory")?;
        let staging = tempfile::Builder::new()
            .prefix(".pchronicle-import-")
            .tempdir_in(parent)
            .with_context(|| format!("create import staging directory in {}", parent.display()))?;
        let (imported_sources, unknown_field_warnings, skipped_warnings) = match output_format {
            ImportOutputFormat::Preserve => {
                let mut unknown_field_warnings =
                    persisting_pchronicle::model::UnknownFieldImportWarnings::default();
                let mut imported_sources = Vec::new();
                let mut skipped_warnings = Vec::new();
                if args.stream {
                    progress.stage(StageId::Fetch).set_current("stdin");
                    let input = read_bounded(stdin, max_input_bytes, "stdin")?;
                    progress.note_fetched("stdin", input.len() as u64)?;
                    progress.stage(StageId::Parse).set_current("stdin");
                    if let Some(source) = stage_preserved_import_source(
                        args.format,
                        args.suggested_format,
                        None,
                        None,
                        None,
                        &input,
                        staging.path(),
                        &mut unknown_field_warnings,
                        &mut skipped_warnings,
                    )? {
                        progress.note_parsed(&source.source_path, source.input_bytes as u64)?;
                        imported_sources.push(source);
                    } else {
                        progress.note_parsed("stdin", input.len() as u64)?;
                    }
                } else {
                    progress
                        .stage(StageId::Discover)
                        .set_total_items(candidates.len() as u64);
                    for candidate in &candidates {
                        let name = candidate.relative_path.to_string_lossy().into_owned();
                        let label = format!("import source {name}");
                        progress.note_discovered(&name, candidate.size_hint)?;
                        progress.stage(StageId::Fetch).set_current(&name);
                        let input =
                            load_import_candidate_bytes(candidate, max_input_bytes, &label).await?;
                        progress.note_fetched(&name, input.len() as u64)?;
                        progress.stage(StageId::Parse).set_current(&name);
                        match stage_preserved_import_source(
                            args.format,
                            args.suggested_format,
                            Some(&candidate.path),
                            Some(&candidate.relative_path),
                            candidate.output_relative_path.as_deref(),
                            &input,
                            staging.path(),
                            &mut unknown_field_warnings,
                            &mut skipped_warnings,
                        ) {
                            Ok(Some(source)) => {
                                progress
                                    .note_parsed(&source.source_path, source.input_bytes as u64)?;
                                imported_sources.push(source);
                            }
                            Ok(None) => {
                                progress.note_parsed(&name, input.len() as u64)?;
                            }
                            Err(error) => {
                                let warning =
                                    skipped_import_warning(Path::new(&name), &format!("{error:#}"));
                                let _ = append_import_log(&name, &error);
                                skipped_warnings.push(warning);
                                progress.note_parsed(&name, input.len() as u64)?;
                            }
                        }
                    }
                }
                (imported_sources, unknown_field_warnings, skipped_warnings)
            }
            ImportOutputFormat::Storyline => {
                unreachable!("storyline import commits in place above")
            }
            ImportOutputFormat::CompactJsonl => unreachable!("compact import handled above"),
        };
        if imported_sources.is_empty() {
            return Err(empty_auto_directory_import_error(directory_input));
        }

        std::fs::File::open(staging.path())
            .and_then(|directory| directory.sync_all())
            .context("sync import staging directory")?;

        let staging_path = staging.keep();
        let mut cleanup = StagingPathGuard::new(staging_path.clone());
        publish_staged_dataset(
            &staging_path,
            &output,
            replace_existing,
            Some(&mut progress),
        )
        .await?;
        cleanup.disarm();
        (
            output.to_string_lossy().into_owned(),
            imported_sources,
            unknown_field_warnings,
            skipped_warnings,
        )
    };
    if imported_sources.is_empty() {
        return Err(empty_auto_directory_import_error(directory_input));
    }
    let trajectories = imported_sources.iter().try_fold(0usize, |total, source| {
        total
            .checked_add(source.trajectories)
            .context("import trajectory count overflow")
    })?;
    let input_bytes = imported_sources.iter().try_fold(0usize, |total, source| {
        total
            .checked_add(source.input_bytes)
            .context("import input byte count overflow")
    })?;
    let on_disk_bytes = measure_storyline_on_disk_bytes(&dataset_uri, output_format).await;
    if let Some(bytes) = on_disk_bytes {
        progress.stage(StageId::Commit).set_bytes(bytes);
        progress
            .stage(StageId::Commit)
            .set_current(format!("on_disk={}", format_byte_count(bytes)));
    }

    let single_source = (!directory_input).then(|| {
        imported_sources
            .first()
            .expect("stdin and regular-file imports have one Source")
    });
    let response = ImportResponse {
        dataset_uri,
        source_path: single_source.map(|source| source.source_path.clone()),
        format: single_source.map(|source| source.format.as_str().to_owned()),
        output_format: output_format.response_name().into(),
        sources: imported_sources.len(),
        trajectories,
        fact_rows: None,
        input_bytes: Some(input_bytes),
        on_disk_bytes,
    };
    serde_json::to_writer_pretty(&mut *stdout, &response)
        .context("encode pChronicle import JSON")?;
    writeln!(stdout).context("write pChronicle import JSON")?;
    progress.finish()?;
    if let (Some(source_path), Some(format)) = (&response.source_path, &response.format) {
        progress.notice(&format!(
            "dataset_uri={} source={} format={} output_format={} trajectories={} input_bytes={}{}",
            response.dataset_uri,
            source_path,
            format,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
            on_disk_bytes_suffix(response.on_disk_bytes),
        ))?;
    } else {
        progress.notice(&format!(
            "dataset_uri={} sources={} output_format={} trajectories={} input_bytes={}{}",
            response.dataset_uri,
            response.sources,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
            on_disk_bytes_suffix(response.on_disk_bytes),
        ))?;
    }
    for line in skipped_warnings {
        progress.notice(&line)?;
    }
    for line in unknown_field_warnings.warning_lines() {
        progress.notice(&line)?;
    }
    progress.flush_log(stderr)?;
    Ok(())
}

/// Keep per-source failures durable while allowing a large import to continue.
pub(crate) fn reset_import_log() -> Result<()> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("import.log")
        .context("reset import.log")?;
    Ok(())
}

pub(crate) fn append_import_log(path: &str, error: &anyhow::Error) -> Result<()> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open("import.log")
        .context("open import.log")?;
    writeln!(log, "source={path}\terror={error:#}").context("append import.log")
}

async fn measure_storyline_on_disk_bytes(
    dataset_uri: &str,
    output_format: ImportOutputFormat,
) -> Option<u64> {
    if output_format != ImportOutputFormat::Storyline {
        // Preserve / other modes may leave non-Storyline trees; skip.
        // Object-store imports always write Storyline even when the CLI
        // defaulted output_format from the destination kind.
        let Ok(location) = DatasetLocation::parse(dataset_uri) else {
            return None;
        };
        if !location.is_object_store() {
            return None;
        }
    }
    match StorylineLanceStore::open_uri(dataset_uri).await {
        Ok(store) => match store.on_disk_bytes().await {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                tracing::warn!(
                    dataset_uri,
                    error = %error,
                    "failed to measure Storyline on-disk bytes after import"
                );
                None
            }
        },
        Err(error) => {
            tracing::warn!(
                dataset_uri,
                error = %error,
                "failed to reopen Storyline Dataset to measure on-disk bytes"
            );
            None
        }
    }
}

fn on_disk_bytes_suffix(on_disk_bytes: Option<u64>) -> String {
    match on_disk_bytes {
        Some(bytes) => format!(" on_disk_bytes={bytes} ({})", format_byte_count(bytes)),
        None => String::new(),
    }
}

pub(crate) async fn run_compact_jsonl_import(
    args: ImportArgs,
    output_arg: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    stderr_is_terminal: bool,
) -> Result<()> {
    anyhow::ensure!(
        args.mode()? != ImportMode::Append,
        "compact JSONL append is not supported; use sync or replace"
    );
    anyhow::ensure!(
        args.from != "-",
        "compact JSONL import does not support stdin"
    );
    let input = Path::new(&args.from);
    let output = Path::new(output_arg);
    anyhow::ensure!(
        !output_arg.starts_with("s3://") && !output_arg.starts_with("oss://"),
        "compact JSONL currently requires local paths"
    );
    if args.mode()? == ImportMode::Create {
        anyhow::ensure!(!output.exists(), "import output already exists");
    }
    let columns = args
        .columns
        .iter()
        .map(|item| {
            let (name, path) = item
                .split_once('=')
                .context("--column must be NAME=JSON_PATH")?;
            persisting_pchronicle::storage::CompactJsonlColumn::new(name.trim(), path.trim())
        })
        .collect::<Result<Vec<_>>>()?;
    let options = persisting_pchronicle::storage::CompactJsonlOptions {
        columns,
        offload_threshold: 4 * 1024 * 1024,
    };
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let staging = tempfile::Builder::new()
        .prefix(".pchronicle-compact-jsonl-")
        .tempdir_in(parent)?;
    let mut progress = CliProgress::new(stderr_is_terminal);
    let _index_progress = progress.attach_index_progress();
    let rows = {
        let progress = &mut progress;
        persisting_pchronicle::storage::CompactJsonlStore::import_path_with_progress(
            input,
            staging.path(),
            &options,
            |event| match event {
                persisting_pchronicle::storage::CompactJsonlImportEvent::Listed {
                    files,
                    bytes,
                } => progress.set_discovered(files, bytes),
                persisting_pchronicle::storage::CompactJsonlImportEvent::Reading {
                    relative,
                    file_bytes,
                    file_rows,
                    total_rows,
                    done,
                } => {
                    let label = format!("{relative} rows={file_rows} total={total_rows}");
                    progress.stage(StageId::Fetch).set_current(label.clone());
                    progress.stage(StageId::Parse).set_current(&label);
                    if done {
                        progress.note_fetched(&relative, file_bytes)?;
                        progress.note_parsed(&relative, file_bytes)?;
                    }
                    Ok(())
                }
                persisting_pchronicle::storage::CompactJsonlImportEvent::Building {
                    phase,
                    rows,
                    processed,
                } => {
                    let commit = progress.stage(StageId::Commit);
                    commit.set_queue_cap(rows);
                    if let Some(processed) = processed {
                        commit.set_queue(processed);
                        commit.set_current(format!("{} {processed}/{rows}", phase.as_str()));
                    } else {
                        commit.set_queue(rows);
                        commit.set_current(format!("{} rows={rows}", phase.as_str()));
                    }
                    Ok(())
                }
                persisting_pchronicle::storage::CompactJsonlImportEvent::Written { rows } => {
                    progress.note_committed(rows, 0)
                }
            },
        )
        .await?
    };
    std::fs::File::open(staging.path())?.sync_all()?;
    let staging_path = staging.keep();
    let mut cleanup = StagingPathGuard::new(staging_path.clone());
    publish_staged_dataset(&staging_path, output, output.exists(), Some(&mut progress)).await?;
    cleanup.disarm();
    progress.finish()?;
    serde_json::to_writer_pretty(
        &mut *stdout,
        &serde_json::json!({"dataset_uri": output_arg, "output_format": "compact-jsonl", "rows": rows}),
    )?;
    writeln!(stdout)?;
    writeln!(
        stderr,
        "dataset_uri={} output_format=compact-jsonl rows={rows}",
        output_arg
    )?;
    progress.flush_log(stderr)?;
    Ok(())
}

pub(crate) struct StorylineImportOptions {
    max_input_bytes: usize,
    directory_input: bool,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    append_generation: Option<String>,
    initial_storyline_count: u64,
    wal: Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
    skip_paths: std::sync::Arc<HashSet<String>>,
}

impl StorylineImportOptions {
    pub(crate) fn create(max_input_bytes: usize, directory_input: bool) -> Self {
        Self {
            max_input_bytes,
            directory_input,
            seen_document_ids: HashSet::new(),
            duplicate_policy: DuplicateIdPolicy::Suffix,
            allow_empty: false,
            append_generation: None,
            initial_storyline_count: 0,
            wal: None,
            skip_paths: std::sync::Arc::new(HashSet::new()),
        }
    }

    pub(crate) fn with_wal(
        mut self,
        wal: Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
        skip_paths: std::sync::Arc<HashSet<String>>,
    ) -> Self {
        self.wal = wal;
        self.skip_paths = skip_paths;
        self
    }
}

pub(crate) async fn squash_storyline_into_store(
    store: &StorylineLanceStore,
    args: &ImportArgs,
    stdin: &mut dyn Read,
    progress: &mut CliProgress,
    candidates: &[ImportFileCandidate],
    object_store_from: Option<DatasetLocation>,
    options: StorylineImportOptions,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let StorylineImportOptions {
        max_input_bytes,
        directory_input,
        seen_document_ids,
        duplicate_policy,
        allow_empty,
        append_generation,
        initial_storyline_count,
        wal,
        skip_paths,
    } = options;
    if args.stream {
        return squash_storyline_stdin_into_store(
            store,
            args.format,
            args.suggested_format,
            max_input_bytes,
            stdin,
            progress,
            seen_document_ids,
            duplicate_policy,
            allow_empty,
            directory_input,
            append_generation,
            initial_storyline_count,
            commit_batch_schedule(args),
        )
        .await;
    }
    let source = match object_store_from {
        Some(location) => ObjectStoreImportSource::Location(location),
        None => ObjectStoreImportSource::Candidates(candidates.to_vec()),
    };
    squash_storyline_files_pipeline(
        store,
        args.format,
        args.suggested_format,
        max_input_bytes,
        progress,
        source,
        seen_document_ids,
        duplicate_policy,
        allow_empty,
        directory_input,
        append_generation,
        initial_storyline_count,
        commit_batch_schedule(args),
        wal,
        skip_paths,
    )
    .await
}

type SharedImportWal = std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>;
type ImportWalSkipSet = std::sync::Arc<HashSet<String>>;

pub(crate) fn open_import_wal(
    args: &ImportArgs,
    from: &str,
    to: &str,
    output_format: ImportOutputFormat,
) -> Result<(Option<SharedImportWal>, ImportWalSkipSet)> {
    let output_name = output_format.response_name();
    let suggested = args
        .suggested_format
        .map(|format| format.as_str().to_string());
    let root = args
        .wal_dir
        .clone()
        .unwrap_or_else(super::wal::ImportWal::default_root);
    let wal = super::wal::ImportWal::open_or_create(
        &root,
        from,
        to,
        output_name,
        suggested.as_deref(),
        args.resume,
        args.reset,
    )?;
    let skip = if args.resume {
        std::sync::Arc::new(wal.skip_paths())
    } else {
        std::sync::Arc::new(HashSet::new())
    };
    Ok((Some(std::sync::Arc::new(std::sync::Mutex::new(wal))), skip))
}

pub(crate) const DEFAULT_COMMIT_BATCH_START: usize = 64;
pub(crate) const DEFAULT_COMMIT_BATCH_MAX: usize = 4096;

#[derive(Debug, Clone)]
pub(crate) struct CommitBatchSchedule {
    pub(crate) next: usize,
    pub(crate) max: usize,
    pub(crate) fixed: bool,
}

impl CommitBatchSchedule {
    pub(crate) fn adaptive() -> Self {
        Self {
            next: DEFAULT_COMMIT_BATCH_START,
            max: DEFAULT_COMMIT_BATCH_MAX,
            fixed: false,
        }
    }

    pub(crate) fn fixed(n: usize) -> Self {
        let n = n.max(1);
        Self {
            next: n,
            max: n,
            fixed: true,
        }
    }

    pub(crate) fn current(&self) -> usize {
        self.next
    }

    pub(crate) fn after_commit(&mut self) {
        if self.fixed {
            return;
        }
        self.next = self.next.saturating_mul(2).min(self.max);
    }
}

pub(crate) fn commit_batch_schedule(args: &ImportArgs) -> CommitBatchSchedule {
    match args.commit_every {
        Some(n) => CommitBatchSchedule::fixed(n),
        None => CommitBatchSchedule::adaptive(),
    }
}

pub(crate) enum ObjectStoreImportSource {
    Candidates(Vec<ImportFileCandidate>),
    Location(DatasetLocation),
}

#[derive(Debug)]
pub(crate) struct CommitStageOutcome {
    pub(crate) imported_sources: Vec<ImportedSource>,
    pub(crate) skipped_warnings: Vec<String>,
    pub(crate) committed_storylines: u64,
    pub(crate) skipped_commit_storylines: usize,
    pub(crate) saw_any: bool,
    pub(crate) discovered_any: bool,
}

pub(crate) struct CommitStageConfig {
    pub(crate) store: StorylineLanceStore,
    pub(crate) commit: StageHandle,
    pub(crate) fetch: StageHandle,
    pub(crate) parse: StageHandle,
    pub(crate) seen_document_ids: HashSet<String>,
    pub(crate) duplicate_policy: DuplicateIdPolicy,
    pub(crate) append_generation: Option<String>,
    pub(crate) initial_storyline_count: u64,
    pub(crate) commit_schedule: CommitBatchSchedule,
    pub(crate) unknown_field_warnings: std::sync::Arc<
        tokio::sync::Mutex<persisting_pchronicle::model::UnknownFieldImportWarnings>,
    >,
    pub(crate) wal: Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
}

struct BatchEntry {
    storyline: StorylineDocument,
    source_path: String,
}

struct SourceCommitTracker {
    /// Remaining storylines not yet successfully committed for each source.
    remaining: std::collections::HashMap<String, u64>,
    totals: std::collections::HashMap<String, u64>,
}

impl SourceCommitTracker {
    fn new() -> Self {
        Self {
            remaining: std::collections::HashMap::new(),
            totals: std::collections::HashMap::new(),
        }
    }

    fn register(&mut self, path: &str, count: u64) {
        if count == 0 {
            return;
        }
        *self.remaining.entry(path.to_owned()).or_insert(0) += count;
        *self.totals.entry(path.to_owned()).or_insert(0) += count;
    }

    fn note_committed(
        &mut self,
        paths: &[String],
        wal: &Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
    ) {
        let mut completed = Vec::new();
        for path in paths {
            if let Some(left) = self.remaining.get_mut(path) {
                *left = left.saturating_sub(1);
                if *left == 0 {
                    completed.push(path.clone());
                }
            }
        }
        if let Some(wal) = wal
            && let Ok(mut guard) = wal.lock()
        {
            for path in &completed {
                let total = self.totals.remove(path).unwrap_or(1);
                self.remaining.remove(path);
                let _ = guard.mark_done(path, total);
            }
        } else {
            for path in &completed {
                self.remaining.remove(path);
                self.totals.remove(path);
            }
        }
    }

    fn note_failed_paths(
        &mut self,
        paths: &[String],
        error: &str,
        wal: &Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
    ) {
        let unique = paths.iter().cloned().collect::<HashSet<_>>();
        for path in &unique {
            self.remaining.remove(path);
            self.totals.remove(path);
        }
        if let Some(wal) = wal
            && let Ok(mut guard) = wal.lock()
        {
            for path in unique {
                let _ = guard.mark_failed(&path, error);
            }
        }
    }
}

/// Single-worker commit stage running on its own tokio task.
///
/// Double-buffers batches: while one batch is writing to storage, keep draining
/// `parsed_rx` into the next batch so parse→commit backpressure does not stall
/// the whole pipeline for the full remote commit latency.
pub(crate) fn spawn_commit_stage(
    mut parsed_rx: tokio::sync::mpsc::Receiver<Result<ParsedItem>>,
    config: CommitStageConfig,
) -> tokio::task::JoinHandle<Result<CommitStageOutcome>> {
    let CommitStageConfig {
        store,
        commit,
        fetch,
        parse,
        mut seen_document_ids,
        duplicate_policy,
        mut append_generation,
        initial_storyline_count,
        mut commit_schedule,
        unknown_field_warnings,
        wal,
    } = config;
    tokio::spawn(async move {
        let mut skipped_warnings = Vec::new();
        let mut imported_sources: Vec<ImportedSource> = Vec::new();
        let mut batch: Vec<BatchEntry> = Vec::with_capacity(commit_schedule.current());
        let mut source_bytes_left = 0u64;
        let mut source_storylines_left = 0u64;
        let mut committed_storylines = 0u64;
        let mut skipped_commit_storylines = 0usize;
        let mut saw_any = false;
        let mut current_source_path = String::new();
        let mut current_storylines = Vec::new().into_iter();
        let mut producer_done = false;
        let mut discovered_any = false;
        let mut inflight: Option<InflightCommitBatch> = None;
        let mut lookahead: VecDeque<Result<ParsedItem>> = VecDeque::new();
        let mut sources = SourceCommitTracker::new();
        refresh_commit_queue(&commit, &commit_schedule, batch.len(), &inflight);

        loop {
            if let Some(mut storyline) = current_storylines.next() {
                saw_any = true;
                if let Some(warning) = apply_duplicate_document_policy(
                    &mut storyline,
                    &mut seen_document_ids,
                    duplicate_policy,
                ) {
                    if warning.contains("skipped") {
                        skipped_warnings.push(warning);
                        let share = take_source_byte_share(
                            &mut source_bytes_left,
                            &mut source_storylines_left,
                        );
                        commit.record_skipped(1, share);
                        sources.note_committed(std::slice::from_ref(&current_source_path), &wal);
                        continue;
                    }
                    skipped_warnings.push(warning);
                }
                let metadata = imported_sources
                    .last_mut()
                    .expect("decoded Storyline has source metadata");
                metadata.trajectories = metadata
                    .trajectories
                    .checked_add(1)
                    .context("import trajectory count overflow")?;
                let share =
                    take_source_byte_share(&mut source_bytes_left, &mut source_storylines_left);
                commit.record_bytes(share);
                batch.push(BatchEntry {
                    storyline,
                    source_path: current_source_path.clone(),
                });
                refresh_commit_queue(&commit, &commit_schedule, batch.len(), &inflight);
                if batch.len() >= commit_schedule.current() {
                    join_inflight_commit_batch_draining(
                        &store,
                        &mut inflight,
                        &mut parsed_rx,
                        &mut lookahead,
                        &mut producer_done,
                        &mut append_generation,
                        &mut committed_storylines,
                        &mut commit_schedule,
                        &mut skipped_commit_storylines,
                        &mut skipped_warnings,
                        &mut sources,
                        &wal,
                    )
                    .await?;
                    inflight = Some(spawn_inflight_commit_batch(
                        store.clone(),
                        commit.clone(),
                        std::mem::take(&mut batch),
                        append_generation.clone(),
                        committed_storylines,
                        initial_storyline_count,
                    ));
                    batch.reserve(commit_schedule.current());
                    refresh_commit_queue(&commit, &commit_schedule, batch.len(), &inflight);
                }
                continue;
            }

            if producer_done && lookahead.is_empty() {
                break;
            }

            let received = if let Some(item) = lookahead.pop_front() {
                Some(item)
            } else {
                commit.enter_upstream_wait();
                let received = parsed_rx.recv().await;
                commit.leave_upstream_wait();
                received
            };
            match received {
                Some(Ok(ParsedItem::Imported {
                    diagnostic_path: _,
                    mut metadata,
                    storylines,
                    warnings,
                })) => {
                    unknown_field_warnings.lock().await.merge(&warnings);
                    discovered_any = true;
                    let storyline_count = storylines.len() as u64;
                    source_bytes_left = metadata.input_bytes as u64;
                    source_storylines_left = storyline_count;
                    current_source_path = metadata.source_path.clone();
                    sources.register(&current_source_path, storyline_count);
                    if storyline_count > 0 {
                        commit.record_inbound(storyline_count);
                    } else if let Some(wal) = &wal
                        && let Ok(mut guard) = wal.lock()
                    {
                        let _ = guard.mark_done(&current_source_path, 0);
                    }
                    metadata.trajectories = 0;
                    imported_sources.push(metadata);
                    current_storylines = storylines.into_iter();
                }
                Some(Ok(ParsedItem::Skipped {
                    path,
                    reason,
                    bytes: _,
                })) => {
                    discovered_any = true;
                    let path_key = path.to_string_lossy().into_owned();
                    let warning = skipped_import_warning(&path, &reason);
                    let _ = append_import_log(&path_key, &anyhow!("{reason}"));
                    if let Some(wal) = &wal
                        && let Ok(mut guard) = wal.lock()
                    {
                        let _ = guard.mark_failed(&path_key, &reason);
                    }
                    skipped_warnings.push(warning);
                }
                Some(Err(error)) => {
                    let _ = join_inflight_commit_batch(
                        &store,
                        &mut inflight,
                        &mut append_generation,
                        &mut committed_storylines,
                        &mut commit_schedule,
                        &mut skipped_commit_storylines,
                        &mut skipped_warnings,
                        &mut sources,
                        &wal,
                    )
                    .await;
                    return Err(error);
                }
                None => {
                    producer_done = true;
                    fetch.clear_current();
                    parse.clear_current();
                }
            }
        }

        join_inflight_commit_batch(
            &store,
            &mut inflight,
            &mut append_generation,
            &mut committed_storylines,
            &mut commit_schedule,
            &mut skipped_commit_storylines,
            &mut skipped_warnings,
            &mut sources,
            &wal,
        )
        .await?;

        if !batch.is_empty() {
            let paths = batch
                .iter()
                .map(|entry| entry.source_path.clone())
                .collect::<Vec<_>>();
            let storylines = batch
                .into_iter()
                .map(|entry| entry.storyline)
                .collect::<Vec<_>>();
            let mut state = StorylineCommitState {
                append_generation: &mut append_generation,
                committed_storylines,
                initial_storyline_count,
                commit_schedule: &mut commit_schedule,
            };
            match commit_or_skip_storyline_import_batch(&store, &commit, storylines, &mut state)
                .await
            {
                Ok(total) => {
                    committed_storylines = total;
                    sources.note_committed(&paths, &wal);
                }
                Err(error) if is_skippable_storyline_commit_error(&error) => {
                    skipped_commit_storylines =
                        skipped_commit_storylines.saturating_add(paths.len());
                    let message = format!("{error:#}");
                    skipped_warnings.push(message.clone());
                    sources.note_failed_paths(&paths, &message, &wal);
                    refresh_append_generation_after_skip(&store, &mut append_generation).await;
                }
                Err(error) => return Err(error),
            }
            refresh_commit_queue(&commit, &commit_schedule, 0, &None);
        }

        commit.clear_current();
        Ok(CommitStageOutcome {
            imported_sources,
            skipped_warnings,
            committed_storylines,
            skipped_commit_storylines,
            saw_any,
            discovered_any,
        })
    })
}

struct InflightCommitBatch {
    handle: tokio::task::JoinHandle<Result<InflightCommitOutcome>>,
    batch_len: usize,
    source_paths: Vec<String>,
}

enum InflightCommitOutcome {
    Committed {
        total: u64,
        generation: Option<String>,
    },
    Skipped {
        error: String,
        batch_len: usize,
    },
}

/// Parsed-item lookahead while both storyline buffers are occupied.
const COMMIT_LOOKAHEAD_CAP: usize = PARSE_TO_COMMIT_BUFFER.saturating_mul(2);

fn refresh_commit_queue(
    commit: &StageHandle,
    schedule: &CommitBatchSchedule,
    filling: usize,
    inflight: &Option<InflightCommitBatch>,
) {
    let inflight_len = inflight.as_ref().map(|job| job.batch_len).unwrap_or(0);
    // Double-buffer capacity: one batch writing + one batch filling.
    let cap = schedule.current().saturating_mul(2) as u64;
    commit.set_queue_cap(cap);
    commit.set_queue((filling + inflight_len) as u64);
}

fn spawn_inflight_commit_batch(
    store: StorylineLanceStore,
    commit: StageHandle,
    batch: Vec<BatchEntry>,
    mut append_generation: Option<String>,
    committed_storylines: u64,
    initial_storyline_count: u64,
) -> InflightCommitBatch {
    let batch_len = batch.len();
    let source_paths = batch
        .iter()
        .map(|entry| entry.source_path.clone())
        .collect::<Vec<_>>();
    let sample_ids = batch
        .iter()
        .take(8)
        .map(|entry| entry.storyline.document_id().to_string())
        .collect::<Vec<_>>();
    let storylines = batch
        .into_iter()
        .map(|entry| entry.storyline)
        .collect::<Vec<_>>();
    let handle = tokio::spawn(async move {
        match commit_storyline_import_batch(
            &store,
            &commit,
            storylines,
            &mut append_generation,
            committed_storylines,
            initial_storyline_count,
        )
        .await
        {
            Ok(total) => Ok(InflightCommitOutcome::Committed {
                total,
                generation: append_generation,
            }),
            Err(error) if is_skippable_storyline_commit_error(&error) => {
                Ok(InflightCommitOutcome::Skipped {
                    error: format!(
                        "storyline commit batch skipped (batch={batch_len}, committed_before={committed_storylines}, sample_document_ids={sample_ids:?}): {error:#}"
                    ),
                    batch_len,
                })
            }
            Err(error) => Err(error),
        }
    });
    InflightCommitBatch {
        handle,
        batch_len,
        source_paths,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_inflight_outcome(
    outcome: InflightCommitOutcome,
    source_paths: &[String],
    append_generation: &mut Option<String>,
    committed_storylines: &mut u64,
    commit_schedule: &mut CommitBatchSchedule,
    skipped_commit_storylines: &mut usize,
    skipped_warnings: &mut Vec<String>,
    sources: &mut SourceCommitTracker,
    wal: &Option<SharedImportWal>,
) -> bool {
    match outcome {
        InflightCommitOutcome::Committed { total, generation } => {
            *append_generation = generation;
            *committed_storylines = total;
            commit_schedule.after_commit();
            sources.note_committed(source_paths, wal);
            false
        }
        InflightCommitOutcome::Skipped { error, batch_len } => {
            *skipped_commit_storylines = skipped_commit_storylines.saturating_add(batch_len);
            skipped_warnings.push(error.clone());
            sources.note_failed_paths(source_paths, &error, wal);
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn join_inflight_commit_batch(
    store: &StorylineLanceStore,
    inflight: &mut Option<InflightCommitBatch>,
    append_generation: &mut Option<String>,
    committed_storylines: &mut u64,
    commit_schedule: &mut CommitBatchSchedule,
    skipped_commit_storylines: &mut usize,
    skipped_warnings: &mut Vec<String>,
    sources: &mut SourceCommitTracker,
    wal: &Option<SharedImportWal>,
) -> Result<()> {
    let Some(job) = inflight.take() else {
        return Ok(());
    };
    let outcome = job
        .handle
        .await
        .context("storyline commit batch task join failed")??;
    let needs_refresh = apply_inflight_outcome(
        outcome,
        &job.source_paths,
        append_generation,
        committed_storylines,
        commit_schedule,
        skipped_commit_storylines,
        skipped_warnings,
        sources,
        wal,
    );
    if needs_refresh {
        refresh_append_generation_after_skip(store, append_generation).await;
    }
    Ok(())
}

/// Join the in-flight write, draining parse→commit into `lookahead` meanwhile.
#[allow(clippy::too_many_arguments)]
async fn join_inflight_commit_batch_draining(
    store: &StorylineLanceStore,
    inflight: &mut Option<InflightCommitBatch>,
    parsed_rx: &mut tokio::sync::mpsc::Receiver<Result<ParsedItem>>,
    lookahead: &mut VecDeque<Result<ParsedItem>>,
    producer_done: &mut bool,
    append_generation: &mut Option<String>,
    committed_storylines: &mut u64,
    commit_schedule: &mut CommitBatchSchedule,
    skipped_commit_storylines: &mut usize,
    skipped_warnings: &mut Vec<String>,
    sources: &mut SourceCommitTracker,
    wal: &Option<SharedImportWal>,
) -> Result<()> {
    let Some(mut job) = inflight.take() else {
        return Ok(());
    };
    loop {
        tokio::select! {
            biased;
            joined = &mut job.handle => {
                let outcome = joined
                    .context("storyline commit batch task join failed")??;
                let needs_refresh = apply_inflight_outcome(
                    outcome,
                    &job.source_paths,
                    append_generation,
                    committed_storylines,
                    commit_schedule,
                    skipped_commit_storylines,
                    skipped_warnings,
                    sources,
                    wal,
                );
                if needs_refresh {
                    refresh_append_generation_after_skip(store, append_generation).await;
                }
                return Ok(());
            }
            item = parsed_rx.recv(), if !*producer_done && lookahead.len() < COMMIT_LOOKAHEAD_CAP => {
                match item {
                    Some(parsed) => {
                        lookahead.push_back(parsed);
                    }
                    None => {
                        *producer_done = true;
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn squash_storyline_files_pipeline(
    store: &StorylineLanceStore,
    requested_format: ExchangeFormat,
    suggested_format: Option<ExchangeFormat>,
    max_input_bytes: usize,
    progress: &mut CliProgress,
    source: ObjectStoreImportSource,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    directory_input: bool,
    append_generation: Option<String>,
    initial_storyline_count: u64,
    commit_schedule: CommitBatchSchedule,
    wal: Option<std::sync::Arc<std::sync::Mutex<super::wal::ImportWal>>>,
    skip_paths: std::sync::Arc<HashSet<String>>,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let ImportPipelineHandles {
        parsed_rx,
        joins,
        unknown_field_warnings,
    } = match source {
        ObjectStoreImportSource::Candidates(candidates) => spawn_candidates_fetch_pipeline(
            candidates,
            ImportPipelineConfig {
                max_input_bytes,
                requested_format,
                suggested_format,
                discover: progress.stage(StageId::Discover),
                fetch: progress.stage(StageId::Fetch),
                parse: progress.stage(StageId::Parse),
                commit: progress.stage(StageId::Commit),
                skip_paths: Arc::clone(&skip_paths),
            },
        ),
        ObjectStoreImportSource::Location(location) => {
            progress
                .stage(StageId::Discover)
                .set_current(location.as_str());
            spawn_location_fetch_pipeline(
                location,
                ImportPipelineConfig {
                    max_input_bytes,
                    requested_format,
                    suggested_format,
                    discover: progress.stage(StageId::Discover),
                    fetch: progress.stage(StageId::Fetch),
                    parse: progress.stage(StageId::Parse),
                    commit: progress.stage(StageId::Commit),
                    skip_paths,
                },
            )
        }
    };

    let commit_join = spawn_commit_stage(
        parsed_rx,
        CommitStageConfig {
            store: store.clone(),
            commit: progress.stage(StageId::Commit),
            fetch: progress.stage(StageId::Fetch),
            parse: progress.stage(StageId::Parse),
            seen_document_ids,
            duplicate_policy,
            append_generation,
            initial_storyline_count,
            commit_schedule,
            unknown_field_warnings: Arc::clone(&unknown_field_warnings),
            wal,
        },
    );

    let commit_result = match commit_join.await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Err(anyhow!("commit stage cancelled")),
        Err(error) => Err(anyhow!("commit stage task failed: {error}")),
    };

    let CommitStageOutcome {
        mut imported_sources,
        skipped_warnings,
        committed_storylines,
        skipped_commit_storylines,
        saw_any,
        discovered_any,
    } = match commit_result {
        Ok(outcome) => {
            join_pipeline_stages(joins).await?;
            outcome
        }
        Err(error) => {
            for join in &joins {
                join.abort();
            }
            let _ = join_pipeline_stages(joins).await;
            return Err(error);
        }
    };

    let unknown_field_warnings = unknown_field_warnings.lock().await.clone();

    retract_imported_trajectories(&mut imported_sources, skipped_commit_storylines);
    if skipped_commit_storylines > 0 {
        imported_sources.retain(|source| source.trajectories > 0);
    }
    if imported_sources.is_empty() {
        if allow_empty && !saw_any {
            return Ok((imported_sources, unknown_field_warnings, skipped_warnings));
        }
        if skipped_commit_storylines > 0 {
            return Err(anyhow!(
                "storyline import committed no trajectories after skipping failed batches"
            ));
        }
        if !discovered_any {
            return Err(cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "import object prefix contains no .json, .jsonl, or .ndjson files",
            ));
        }
        return Err(empty_auto_directory_import_error(directory_input));
    }
    anyhow::ensure!(
        store.current_table_paths().await?.is_some(),
        "squashed Storyline Lance Dataset has no committed snapshot"
    );
    let imported_trajectories = imported_sources.iter().try_fold(0usize, |total, source| {
        total
            .checked_add(source.trajectories)
            .context("import trajectory count overflow")
    })?;
    anyhow::ensure!(
        committed_storylines as usize == imported_trajectories,
        "squashed Storyline import report does not match decoded trajectory count"
    );
    finalize_storyline_import_indexes(store, progress).await?;
    Ok((imported_sources, unknown_field_warnings, skipped_warnings))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn squash_storyline_stdin_into_store(
    store: &StorylineLanceStore,
    requested_format: ExchangeFormat,
    suggested_format: Option<ExchangeFormat>,
    max_input_bytes: usize,
    stdin: &mut dyn Read,
    progress: &mut CliProgress,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    directory_input: bool,
    append_generation: Option<String>,
    initial_storyline_count: u64,
    commit_schedule: CommitBatchSchedule,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let import = StorylineImportIterator::stdin(
        requested_format,
        suggested_format,
        max_input_bytes,
        stdin,
        progress,
        seen_document_ids,
        duplicate_policy,
    );
    drain_storyline_import_batches(
        store,
        import,
        append_generation,
        commit_schedule,
        allow_empty,
        directory_input,
        initial_storyline_count,
    )
    .await
}

pub(crate) async fn drain_storyline_import_batches(
    store: &StorylineLanceStore,
    mut import: StorylineImportIterator<'_>,
    mut append_generation: Option<String>,
    mut commit_schedule: CommitBatchSchedule,
    allow_empty: bool,
    directory_input: bool,
    initial_storyline_count: u64,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let mut batch = Vec::with_capacity(commit_schedule.current());
    let mut committed_storylines = 0u64;
    let mut skipped_commit_storylines = 0usize;
    let mut commit_skip_warnings = Vec::new();
    let mut saw_any = false;

    loop {
        match import.next_document().await {
            Some(item) => {
                saw_any = true;
                batch.push(item?);
                if batch.len() < commit_schedule.current() {
                    continue;
                }
                let batch_len = batch.len() as u64;
                let commit = import.progress.stage(StageId::Commit);
                let mut state = StorylineCommitState {
                    append_generation: &mut append_generation,
                    committed_storylines,
                    initial_storyline_count,
                    commit_schedule: &mut commit_schedule,
                };
                match commit_or_skip_storyline_import_batch(
                    store,
                    &commit,
                    std::mem::take(&mut batch),
                    &mut state,
                )
                .await
                {
                    Ok(total) => {
                        committed_storylines = total;
                    }
                    Err(error) if is_skippable_storyline_commit_error(&error) => {
                        skipped_commit_storylines =
                            skipped_commit_storylines.saturating_add(batch_len as usize);
                        commit_skip_warnings.push(format!("{error:#}"));
                        refresh_append_generation_after_skip(store, &mut append_generation).await;
                    }
                    Err(error) => return Err(error),
                }
                batch.reserve(commit_schedule.current());
            }
            None if batch.is_empty() => break,
            None => {
                let batch_len = batch.len() as u64;
                let commit = import.progress.stage(StageId::Commit);
                let mut state = StorylineCommitState {
                    append_generation: &mut append_generation,
                    committed_storylines,
                    initial_storyline_count,
                    commit_schedule: &mut commit_schedule,
                };
                match commit_or_skip_storyline_import_batch(
                    store,
                    &commit,
                    std::mem::take(&mut batch),
                    &mut state,
                )
                .await
                {
                    Ok(total) => {
                        committed_storylines = total;
                    }
                    Err(error) if is_skippable_storyline_commit_error(&error) => {
                        skipped_commit_storylines =
                            skipped_commit_storylines.saturating_add(batch_len as usize);
                        commit_skip_warnings.push(format!("{error:#}"));
                        refresh_append_generation_after_skip(store, &mut append_generation).await;
                    }
                    Err(error) => return Err(error),
                }
                break;
            }
        }
    }

    let (mut imported_sources, unknown_field_warnings, mut skipped_warnings, progress) =
        import.into_result_parts();
    skipped_warnings.extend(commit_skip_warnings);
    retract_imported_trajectories(&mut imported_sources, skipped_commit_storylines);
    if skipped_commit_storylines > 0 {
        imported_sources.retain(|source| source.trajectories > 0);
    }
    if imported_sources.is_empty() {
        if allow_empty && !saw_any {
            return Ok((imported_sources, unknown_field_warnings, skipped_warnings));
        }
        if skipped_commit_storylines > 0 {
            return Err(anyhow!(
                "storyline import committed no trajectories after skipping failed batches"
            ));
        }
        return Err(empty_auto_directory_import_error(directory_input));
    }
    anyhow::ensure!(
        store.current_table_paths().await?.is_some(),
        "squashed Storyline Lance Dataset has no committed snapshot"
    );
    let imported_trajectories = imported_sources.iter().try_fold(0usize, |total, source| {
        total
            .checked_add(source.trajectories)
            .context("import trajectory count overflow")
    })?;
    anyhow::ensure!(
        committed_storylines as usize == imported_trajectories,
        "squashed Storyline import report does not match decoded trajectory count"
    );
    finalize_storyline_import_indexes(store, progress).await?;
    Ok((imported_sources, unknown_field_warnings, skipped_warnings))
}

pub(crate) fn is_skippable_storyline_commit_error(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("timeout")
        || text.contains("timed out")
        || text.contains("error sending request")
        || text.contains("conditionnotmatch")
        || text.contains("preconditionfailed")
        || text.contains("precondition failed")
        || text.contains("throttle")
        || text.contains("slow down")
        || text.contains("503")
        || text.contains("429")
        || text.contains("connection reset")
        || text.contains("broken pipe")
        || text.contains("lanceerror(io)")
        || text.contains("generic s3 error")
        || text.contains("client error (connect)")
        || text.contains("byte array offset overflow")
        || text.contains("arrow encode panicked")
        || text.contains("max_chunk_bytes")
        || text.contains("max_document_bytes")
        || text.contains("max_chunk_rows")
        || text.contains("max_document_rows")
}

pub(crate) fn retract_imported_trajectories(sources: &mut [ImportedSource], mut count: usize) {
    for source in sources.iter_mut().rev() {
        if count == 0 {
            break;
        }
        let take = source.trajectories.min(count);
        source.trajectories -= take;
        count -= take;
    }
}

pub(crate) async fn refresh_append_generation_after_skip(
    store: &StorylineLanceStore,
    append_generation: &mut Option<String>,
) {
    match store.current_table_paths().await {
        Ok(Some(paths)) => {
            *append_generation = Some(paths.generation);
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                root = %store.root_uri(),
                error = %error,
                "failed to refresh Storyline generation after skipped commit batch"
            );
        }
    }
}

pub(crate) fn take_source_byte_share(bytes_left: &mut u64, storylines_left: &mut u64) -> u64 {
    if *storylines_left == 0 {
        return 0;
    }
    let share = if *storylines_left == 1 {
        *bytes_left
    } else {
        *bytes_left / *storylines_left
    };
    *bytes_left = bytes_left.saturating_sub(share);
    *storylines_left = storylines_left.saturating_sub(1);
    share
}

pub(crate) struct StorylineCommitState<'a> {
    pub(crate) append_generation: &'a mut Option<String>,
    pub(crate) committed_storylines: u64,
    pub(crate) initial_storyline_count: u64,
    pub(crate) commit_schedule: &'a mut CommitBatchSchedule,
}

pub(crate) async fn commit_or_skip_storyline_import_batch(
    store: &StorylineLanceStore,
    commit: &StageHandle,
    batch: Vec<StorylineDocument>,
    state: &mut StorylineCommitState<'_>,
) -> Result<u64> {
    let batch_len = batch.len() as u64;
    let sample_ids = batch
        .iter()
        .take(8)
        .map(|storyline| storyline.document_id().to_string())
        .collect::<Vec<_>>();
    match commit_storyline_import_batch(
        store,
        commit,
        batch,
        state.append_generation,
        state.committed_storylines,
        state.initial_storyline_count,
    )
    .await
    {
        Ok(total) => {
            state.commit_schedule.after_commit();
            Ok(total)
        }
        Err(error) if is_skippable_storyline_commit_error(&error) => Err(error).context(
            format!(
                "storyline commit batch failed after transient storage error (batch={batch_len}, committed_before={}, sample_document_ids={sample_ids:?})",
                state.committed_storylines
            ),
        ),
        Err(error) => Err(error),
    }
}

pub(crate) async fn finalize_storyline_import_indexes(
    store: &StorylineLanceStore,
    progress: &mut CliProgress,
) -> Result<()> {
    progress
        .stage(StageId::Commit)
        .set_current("optimize indices (final)");
    let _index_progress = progress.attach_index_progress();
    store
        .maintain(&persisting_pchronicle::storage::LanceMaintenanceOptions {
            compact: false,
            optimize_indices: true,
            vacuum_older_than: None,
            ..Default::default()
        })
        .await
        .context("finalize Storyline indexes after progressive import")?;
    progress
        .stage(StageId::Commit)
        .set_current("optimize indices done");
    Ok(())
}

pub(crate) async fn commit_storyline_import_batch(
    store: &StorylineLanceStore,
    commit: &StageHandle,
    batch: Vec<StorylineDocument>,
    append_generation: &mut Option<String>,
    committed_storylines: u64,
    initial_storyline_count: u64,
) -> Result<u64> {
    anyhow::ensure!(!batch.is_empty(), "storyline import commit batch is empty");
    let batch_len = batch.len() as u64;
    commit.set_current(format!("batch={batch_len}"));
    let report = match append_generation.as_deref() {
        Some(generation) => {
            tracing::info!(
                committed_before = committed_storylines,
                batch_len,
                expected_generation = generation,
                root = %store.root_uri(),
                "storyline progressive append commit starting"
            );
            store
                .append_storyline_stream_with_options(
                    batch.into_iter().map(Ok),
                    generation,
                    persisting_pchronicle::storage::StorylineStreamOptions::defer_index_optimize(),
                )
                .await
                .with_context(|| {
                    format!(
                        "storyline progressive append commit failed (committed_before={committed_storylines}, batch={batch_len}, expected_generation={generation}, root={})",
                        store.root_uri()
                    )
                })?
        }
        None => {
            tracing::info!(
                batch_len,
                root = %store.root_uri(),
                "storyline progressive replace commit starting"
            );
            store
                .replace_storyline_stream_with_options(
                    batch.into_iter().map(Ok),
                    persisting_pchronicle::storage::StorylineStreamOptions::defer_index_optimize(),
                )
                .await
                .with_context(|| {
                    format!(
                        "storyline progressive replace commit failed (batch={batch_len}, root={})",
                        store.root_uri()
                    )
                })?
        }
    };
    anyhow::ensure!(
        report.storylines as u64 == batch_len,
        "storyline import batch report does not match batch size"
    );
    let paths = store
        .current_table_paths()
        .await?
        .context("storyline import batch produced no committed snapshot")?;
    let imported_total = committed_storylines
        .checked_add(batch_len)
        .context("import trajectory count overflow")?;
    let manifest_total = initial_storyline_count
        .checked_add(imported_total)
        .context("import manifest record count overflow")?;
    persisting_pchronicle::storage::write_storyline_manifest_at_uri(
        store.root_uri(),
        &paths.generation,
        manifest_total,
        0,
    )
    .await
    .context("write progressive chronicle.manifest after storyline commit")?;
    *append_generation = Some(paths.generation.clone());
    // Bytes were already attributed when trajectories entered the batch.
    commit.note_committed(imported_total, 0);
    Ok(imported_total)
}

pub(crate) async fn run_canonical_event_import(
    args: ImportArgs,
    _snapshot: EventFactSnapshot,
    destination: DatasetLocation,
    replace_existing: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.format == ExchangeFormat::Auto,
        "canonical event import does not accept a JSON exchange --format"
    );
    anyhow::ensure!(
        args.output_format != Some(ImportOutputFormat::Preserve),
        "canonical event import cannot preserve an existing canonical event Store"
    );
    if destination.exists().await? && !replace_existing {
        return Err(cli_boundary_error(
            BoundaryCode::Conflict,
            "import output already exists",
        ));
    }
    let output_uri = destination.as_str().to_string();

    let (report, staged_path) = if replace_existing {
        let output = destination
            .local_path()
            .context("replace import output must be a local Dataset path")?;
        let parent = output
            .parent()
            .context("replace import output must have a parent directory")?;
        let staging = tempfile::Builder::new()
            .prefix(".pchronicle-import-")
            .tempdir_in(parent)
            .with_context(|| format!("create import staging directory in {}", parent.display()))?;
        let staging_uri = staging.path().to_string_lossy().into_owned();
        let report =
            match build_storyline_projection(&args.from, &staging_uri, "events.lance").await? {
                StorylineProjectionBuildOutcome::Built(report) => report,
                StorylineProjectionBuildOutcome::OutputNotEmpty => {
                    return Err(cli_boundary_error(
                        BoundaryCode::Conflict,
                        "import staging Dataset already exists",
                    ));
                }
            };
        std::fs::File::open(staging.path())
            .and_then(|directory| directory.sync_all())
            .context("sync import staging directory")?;
        (report, Some((staging.keep(), output.to_path_buf())))
    } else {
        let report =
            match build_storyline_projection(&args.from, &output_uri, "events.lance").await? {
                StorylineProjectionBuildOutcome::Built(report) => report,
                StorylineProjectionBuildOutcome::OutputNotEmpty => {
                    return Err(cli_boundary_error(
                        BoundaryCode::Conflict,
                        "import output already exists",
                    ));
                }
            };
        (report, None)
    };
    if let Some((staging_path, output)) = staged_path {
        let mut cleanup = StagingPathGuard::new(staging_path.clone());
        publish_staged_dataset(&staging_path, &output, true, None).await?;
        cleanup.disarm();
    }
    let response = ImportResponse {
        dataset_uri: output_uri,
        source_path: Some("events.lance".into()),
        format: Some("events".into()),
        output_format: ImportOutputFormat::Storyline.response_name().into(),
        sources: 1,
        trajectories: report.storylines,
        fact_rows: Some(report.fact_rows),
        input_bytes: None,
        on_disk_bytes: None,
    };
    serde_json::to_writer_pretty(&mut *stdout, &response)
        .context("encode canonical event import JSON")?;
    writeln!(stdout).context("write canonical event import JSON")?;
    writeln!(
        stderr,
        "dataset_uri={} source=events.lance format=events output_format={} trajectories={} fact_rows={}",
        response.dataset_uri,
        response.output_format,
        response.trajectories,
        report.fact_rows,
    )
    .context("write canonical event import metadata")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_pchronicle::document::DocumentFormat;

    #[test]
    fn commit_batch_schedule_grows_to_cap() {
        let mut schedule = CommitBatchSchedule::adaptive();
        assert_eq!(schedule.current(), 64);
        schedule.after_commit();
        assert_eq!(schedule.current(), 128);
        schedule.after_commit();
        assert_eq!(schedule.current(), 256);
        schedule.after_commit();
        assert_eq!(schedule.current(), 512);
        schedule.after_commit();
        assert_eq!(schedule.current(), 1024);
        schedule.after_commit();
        assert_eq!(schedule.current(), 2048);
        schedule.after_commit();
        assert_eq!(schedule.current(), 4096);
        schedule.after_commit();
        assert_eq!(schedule.current(), 4096);
    }

    #[test]
    fn skippable_commit_errors_cover_s3_timeouts_and_preconditions() {
        assert!(is_skippable_storyline_commit_error(&anyhow!(
            "LanceError(IO): Generic S3 error: operation timed out"
        )));
        assert!(is_skippable_storyline_commit_error(&anyhow!(
            "ConditionNotMatch (persistent) PreconditionFailed"
        )));
        assert!(is_skippable_storyline_commit_error(&anyhow!(
            "arrow encode panicked: byte array offset overflow"
        )));
        assert!(is_skippable_storyline_commit_error(&anyhow!(
            "document exceeds max_chunk_bytes"
        )));
        assert!(!is_skippable_storyline_commit_error(&anyhow!(
            "duplicate document_id policy rejected payload"
        )));
    }

    #[test]
    fn open_import_wal_skips_only_on_resume() {
        let root = tempfile::tempdir().unwrap();
        let mut base = ImportArgs {
            from: "s3://bucket/from".into(),
            output: Some("s3://bucket/to".into()),
            format: ExchangeFormat::Auto,
            suggested_format: None,
            output_format: Some(ImportOutputFormat::Storyline),
            replace: false,
            append: false,
            on_duplicate: None,
            yes: true,
            stream: false,
            max_input_bytes: None,
            commit_every: None,
            resume: false,
            wal_dir: Some(root.path().to_path_buf()),
            reset: false,
            columns: Vec::new(),
        };
        let (wal, skip) = open_import_wal(
            &base,
            &base.from,
            "s3://bucket/to",
            ImportOutputFormat::Storyline,
        )
        .unwrap();
        assert!(skip.is_empty());
        {
            let mut guard = wal.as_ref().unwrap().lock().unwrap();
            guard.mark_done("done.json", 1).unwrap();
            guard.mark_failed("fail.json", "parse").unwrap();
        }

        let (_, skip_again) = open_import_wal(
            &base,
            &base.from,
            "s3://bucket/to",
            ImportOutputFormat::Storyline,
        )
        .unwrap();
        assert!(
            skip_again.is_empty(),
            "without --resume, prior WAL entries must not be skipped"
        );

        base.resume = true;
        let (_, skip_resume) = open_import_wal(
            &base,
            &base.from,
            "s3://bucket/to",
            ImportOutputFormat::Storyline,
        )
        .unwrap();
        assert!(skip_resume.contains("done.json"));
        assert!(skip_resume.contains("fail.json"));
    }

    #[test]
    fn source_commit_tracker_marks_done_when_all_storylines_commit() {
        let root = tempfile::tempdir().unwrap();
        let wal = super::super::wal::ImportWal::open_or_create(
            root.path(),
            "from",
            "to",
            "storyline-lance",
            None,
            false,
            false,
        )
        .unwrap();
        let wal = std::sync::Arc::new(std::sync::Mutex::new(wal));
        let mut tracker = SourceCommitTracker::new();
        tracker.register("a.json", 2);
        tracker.note_committed(&[String::from("a.json")], &Some(wal.clone()));
        assert!(!wal.lock().unwrap().should_skip("a.json"));
        tracker.note_committed(&[String::from("a.json")], &Some(wal.clone()));
        assert!(wal.lock().unwrap().should_skip("a.json"));
    }

    #[test]
    fn retract_imported_trajectories_from_tail_sources() {
        let mut sources = vec![
            ImportedSource {
                source_path: "a.json".into(),
                format: DocumentFormat::Atif,
                trajectories: 3,
                input_bytes: 10,
            },
            ImportedSource {
                source_path: "b.json".into(),
                format: DocumentFormat::Atif,
                trajectories: 2,
                input_bytes: 10,
            },
        ];
        retract_imported_trajectories(&mut sources, 3);
        assert_eq!(sources[0].trajectories, 2);
        assert_eq!(sources[1].trajectories, 0);
    }

    #[test]
    fn commit_batch_schedule_fixed_stays_put() {
        let mut schedule = CommitBatchSchedule::fixed(50);
        assert_eq!(schedule.current(), 50);
        schedule.after_commit();
        assert_eq!(schedule.current(), 50);
    }
}
