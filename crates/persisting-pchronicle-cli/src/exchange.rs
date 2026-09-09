use super::*;

#[derive(Serialize)]
struct DropResponse {
    dataset_uri: String,
    dropped: bool,
}

pub(super) async fn run_drop(
    args: DropArgs,
    settings_override: Option<&Path>,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let dataset_uri = expand_dataset_reference(&args.dataset_uri, settings_override, false)?;
    let mut location = DatasetLocation::parse(&dataset_uri)?;
    if !location.exists().await? {
        return Err(cli_boundary_error(
            BoundaryCode::NotFound,
            format!("Dataset does not exist: {}", location.as_str()),
        ));
    }
    if location.local_path().is_some() {
        location = location.into_existing()?;
    }
    confirm_destructive_dataset(
        "drop",
        location.as_str(),
        args.yes,
        stdin_is_terminal,
        stdin,
        stderr,
    )?;
    location.remove_all().await?;
    let response = DropResponse {
        dataset_uri: location.as_str().to_string(),
        dropped: true,
    };
    serde_json::to_writer_pretty(&mut *stdout, &response).context("encode pChronicle drop JSON")?;
    writeln!(stdout).context("write pChronicle drop JSON")?;
    writeln!(
        stderr,
        "dataset_uri={} status=dropped",
        response.dataset_uri
    )
    .context("write pChronicle drop metadata")?;
    Ok(())
}

async fn prepare_import_destination(
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

struct PreparedImportDestination {
    location: DatasetLocation,
    replace_existing: bool,
}

fn ensure_import_source_outside_destination(
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

fn confirm_destructive_dataset(
    action: &str,
    dataset_uri: &str,
    yes: bool,
    stdin_is_terminal: bool,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{action} requires confirmation; rerun with --yes"),
        ));
    }
    write!(
        stderr,
        "Permanently {action} Dataset '{dataset_uri}'? [y/N] "
    )
    .context("write Dataset confirmation prompt")?;
    stderr
        .flush()
        .context("flush Dataset confirmation prompt")?;
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while answer.len() <= 16 && stdin.read(&mut byte).context("read Dataset confirmation")? == 1 {
        if byte[0] == b'\n' {
            break;
        }
        answer.push(byte[0]);
    }
    let answer = std::str::from_utf8(&answer)
        .context("Dataset confirmation is not UTF-8")?
        .trim();
    if matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    Err(cli_boundary_error(
        BoundaryCode::InvalidRequest,
        format!("{action} cancelled"),
    ))
}

pub(super) async fn run_import(
    mut args: ImportArgs,
    settings_override: Option<&Path>,
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    args.stream = args.from == "-" || args.stream;
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
        return run_compact_jsonl_import(args, &output_arg, stdout, stderr).await;
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
    let mut progress = ImportProgress::new(stderr_is_terminal);
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
    let output_format = args
        .output_format
        .unwrap_or(if mode == ImportMode::Append {
            ImportOutputFormat::Storyline
        } else {
            ImportOutputFormat::Preserve
        });
    let duplicate_policy = args.on_duplicate.unwrap_or(DuplicateIdPolicy::Suffix);
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
        // Remote object-store targets stage locally first: Lance index builds
        // on S3 are extremely slow, so we write+index on disk then upload.
        if destination.exists().await? {
            if replace_existing {
                destination
                    .remove_all_with_progress(|deleted, total, path| {
                        progress.note_deleted(deleted, total, path)
                    })
                    .await
                    .with_context(|| {
                        format!("remove existing Dataset {}", destination.as_str())
                    })?;
                progress.finish()?;
                // Delete progress reuses the paint lines but must not wipe discovery
                // totals collected before replace (local candidates only).
                progress.reset_import_counters();
                if object_store_from.is_none() {
                    let discovered_bytes = candidates.iter().try_fold(0u64, |total, candidate| {
                        total
                            .checked_add(candidate.size_hint)
                            .context("import discovered byte count overflow")
                    })?;
                    progress.set_discovered(candidates.len() as u64, discovered_bytes)?;
                }
            } else {
                return Err(cli_boundary_error(
                    BoundaryCode::Conflict,
                    "import output already exists",
                ));
            }
        }
        let (imported_sources, unknown_field_warnings, skipped_warnings) =
            if destination.is_object_store() {
                progress.set_phase(ImportPhase::Writing, "local staging (indexes on disk)")?;
                let staging = tempfile::Builder::new()
                    .prefix("pchronicle-storyline-stage-")
                    .tempdir()
                    .context("create local Storyline staging directory")?;
                let store = StorylineLanceStore::open(staging.path())
                    .await
                    .context("open local Storyline staging Dataset")?;
                let result = squash_storyline_into_store(
                    &store,
                    &args,
                    stdin,
                    &mut progress,
                    &candidates,
                    object_store_from.clone(),
                    StorylineImportOptions::create(max_input_bytes, directory_input),
                )
                .await?;
                upload_local_storyline_dataset(staging.path(), &destination, &mut progress)
                    .await
                    .with_context(|| {
                        format!(
                            "upload staged Storyline Dataset to {}",
                            destination.as_str()
                        )
                    })?;
                result
            } else {
                let store = StorylineLanceStore::open_uri(destination.as_str())
                    .await
                    .context("create squashed Storyline Lance Dataset")?;
                squash_storyline_into_store(
                    &store,
                    &args,
                    stdin,
                    &mut progress,
                    &candidates,
                    object_store_from.clone(),
                    StorylineImportOptions::create(max_input_bytes, directory_input),
                )
                .await?
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
                    progress.set_phase(ImportPhase::Reading, "stdin")?;
                    let input = read_bounded(stdin, max_input_bytes, "stdin")?;
                    progress.set_phase(ImportPhase::Parsing, "stdin")?;
                    if let Some(source) = stage_preserved_import_source(
                        args.format,
                        None,
                        None,
                        None,
                        &input,
                        staging.path(),
                        &mut unknown_field_warnings,
                        &mut skipped_warnings,
                    )? {
                        progress.set_phase(ImportPhase::Writing, &source.source_path)?;
                        progress.note_imported(source.input_bytes as u64)?;
                        imported_sources.push(source);
                    } else {
                        progress.note_imported(input.len() as u64)?;
                    }
                } else {
                    for candidate in &candidates {
                        let name = candidate.relative_path.to_string_lossy();
                        let label = format!("import source {name}");
                        progress.set_phase(ImportPhase::Reading, &name)?;
                        let input =
                            load_import_candidate_bytes(candidate, max_input_bytes, &label).await?;
                        progress.set_phase(ImportPhase::Parsing, &name)?;
                        if let Some(source) = stage_preserved_import_source(
                            args.format,
                            Some(&candidate.path),
                            Some(&candidate.relative_path),
                            candidate.output_relative_path.as_deref(),
                            &input,
                            staging.path(),
                            &mut unknown_field_warnings,
                            &mut skipped_warnings,
                        )? {
                            progress.set_phase(ImportPhase::Writing, &source.source_path)?;
                            progress.note_imported(source.input_bytes as u64)?;
                            imported_sources.push(source);
                        } else {
                            progress.note_imported(input.len() as u64)?;
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
        publish_staged_dataset(&staging_path, &output, replace_existing, Some(&mut progress)).await?;
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
    };
    serde_json::to_writer_pretty(&mut *stdout, &response)
        .context("encode pChronicle import JSON")?;
    writeln!(stdout).context("write pChronicle import JSON")?;
    progress.finish()?;
    if let (Some(source_path), Some(format)) = (&response.source_path, &response.format) {
        progress.notice(&format!(
            "dataset_uri={} source={} format={} output_format={} trajectories={} input_bytes={}",
            response.dataset_uri,
            source_path,
            format,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
        ))?;
    } else {
        progress.notice(&format!(
            "dataset_uri={} sources={} output_format={} trajectories={} input_bytes={}",
            response.dataset_uri,
            response.sources,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
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

async fn run_compact_jsonl_import(
    args: ImportArgs,
    output_arg: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
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
    let rows = persisting_pchronicle::storage::CompactJsonlStore::import_path(
        input,
        staging.path(),
        &options,
    )
    .await?;
    std::fs::File::open(staging.path())?.sync_all()?;
    let staging_path = staging.keep();
    let mut cleanup = StagingPathGuard::new(staging_path.clone());
    publish_staged_dataset(&staging_path, output, output.exists(), None).await?;
    cleanup.disarm();
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
    Ok(())
}

/// Run one full snapshot import for the resident sync worker.
///
/// The existing import path already stages local outputs atomically, mirrors
/// deletions, and rebuilds a Storyline Lance destination from the same source
/// directory. Keeping the orchestration here avoids a second decoder or
/// Dataset publication protocol in the sync command.
pub(crate) async fn sync_snapshot(
    source: &str,
    warehouse: &str,
    storyline: &str,
    input_format: ExchangeFormat,
    columns: &[String],
) -> Result<()> {
    if input_format == ExchangeFormat::CompactJsonl {
        let mut stdout = std::io::sink();
        let mut stderr = std::io::sink();
        return run_compact_jsonl_import(
            ImportArgs {
                from: source.to_owned(),
                output: Some(storyline.to_owned()),
                format: ExchangeFormat::CompactJsonl,
                output_format: Some(ImportOutputFormat::CompactJsonl),
                replace: true,
                append: false,
                mode: None,
                on_duplicate: None,
                yes: true,
                stream: false,
                max_input_bytes: Some(256 * 1024 * 1024),
                commit_every: None,
                columns: columns.to_vec(),
            },
            storyline,
            &mut stdout,
            &mut stderr,
        )
        .await;
    }
    // ponytail: rebuild one atomic snapshot per coalesced batch; add affected-document mutation
    // when profiling shows full-directory rebuilds are the bottleneck.
    let mut stdout = std::io::sink();
    let mut stderr = std::io::sink();
    let mut stdin = std::io::empty();
    run_import(
        ImportArgs {
            from: source.to_owned(),
            output: Some(warehouse.to_owned()),
            format: input_format,
            output_format: Some(ImportOutputFormat::Preserve),
            replace: true,
            append: false,
            mode: None,
            on_duplicate: None,
            yes: true,
            stream: false,
            max_input_bytes: Some(256 * 1024 * 1024),
            commit_every: None,
            columns: Vec::new(),
        },
        None,
        false,
        false,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
    .await
    .context("sync source into Warehouse")?;
    run_import(
        ImportArgs {
            from: source.to_owned(),
            output: Some(storyline.to_owned()),
            format: input_format,
            output_format: Some(ImportOutputFormat::Storyline),
            replace: true,
            append: false,
            mode: None,
            on_duplicate: None,
            yes: true,
            stream: false,
            max_input_bytes: Some(256 * 1024 * 1024),
            commit_every: None,
            columns: Vec::new(),
        },
        None,
        false,
        false,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
    .await
    .context("sync source into Storyline Lance")?;
    Ok(())
}

struct StorylineImportOptions {
    max_input_bytes: usize,
    directory_input: bool,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    append_generation: Option<String>,
}

impl StorylineImportOptions {
    fn create(max_input_bytes: usize, directory_input: bool) -> Self {
        Self {
            max_input_bytes,
            directory_input,
            seen_document_ids: HashSet::new(),
            duplicate_policy: DuplicateIdPolicy::Suffix,
            allow_empty: false,
            append_generation: None,
        }
    }
}

/// How many Sources the reader may prefetch ahead of parse/write.
/// Bounded so large object-store imports do not buffer unbounded memory.
const IMPORT_READ_AHEAD: usize = 3;
/// Pipeline channel capacity for object-store discover/read events. Listing
/// emits Discovered first; this buffer only absorbs Loaded messages while a
/// commit is in flight.
const IMPORT_PIPELINE_CHANNEL: usize = 16;

struct PipelineLoadedSource {
    candidate: ImportFileCandidate,
    bytes: Vec<u8>,
}

enum PipelineMsg {
    Scanning(String),
    Discovered { path: String, bytes: u64 },
    Loaded(PipelineLoadedSource),
}

fn spawn_candidates_load_producer(
    candidates: Vec<ImportFileCandidate>,
    max_input_bytes: usize,
    reading_ahead: Arc<std::sync::Mutex<String>>,
) -> (
    tokio::sync::mpsc::Receiver<Result<PipelineMsg>>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<PipelineMsg>>(IMPORT_READ_AHEAD);
    let producer = tokio::spawn(async move {
        for candidate in candidates {
            let name = candidate.relative_path.to_string_lossy().into_owned();
            if let Ok(mut guard) = reading_ahead.lock() {
                *guard = name.clone();
            }
            let label = format!("import source {name}");
            let loaded = match load_import_candidate_bytes(&candidate, max_input_bytes, &label).await
            {
                Ok(bytes) => Ok(PipelineMsg::Loaded(PipelineLoadedSource { candidate, bytes })),
                Err(error) => Err(error),
            };
            if tx.send(loaded).await.is_err() {
                return;
            }
        }
        if let Ok(mut guard) = reading_ahead.lock() {
            guard.clear();
        }
    });
    (rx, producer)
}

fn spawn_object_store_discover_load_producer(
    location: DatasetLocation,
    max_input_bytes: usize,
    reading_ahead: Arc<std::sync::Mutex<String>>,
) -> (
    tokio::sync::mpsc::Receiver<Result<PipelineMsg>>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<PipelineMsg>>(IMPORT_PIPELINE_CHANNEL);
    let producer = tokio::spawn(async move {
        let remote_root = location.as_str().to_owned();
        // List completely before any Load so discovery totals keep moving even
        // when a later commit/index stalls the consumer.
        let pending_files = Arc::new(std::sync::Mutex::new(Vec::<(String, u64)>::new()));
        let list_result = location
            .for_each_importable_json_object_event(
                persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES,
                |event| {
                    let tx = tx.clone();
                    let pending_files = Arc::clone(&pending_files);
                    async move {
                        match event {
                            persisting_pchronicle::storage::ImportableObjectEvent::Scanning {
                                prefix,
                            } => {
                                let _ = tx.send(Ok(PipelineMsg::Scanning(prefix))).await;
                                Ok(())
                            }
                            persisting_pchronicle::storage::ImportableObjectEvent::File {
                                key,
                                size,
                                ..
                            } => {
                                if tx
                                    .send(Ok(PipelineMsg::Discovered {
                                        path: key.clone(),
                                        bytes: size,
                                    }))
                                    .await
                                    .is_err()
                                {
                                    return Ok(());
                                }
                                if let Ok(mut guard) = pending_files.lock() {
                                    guard.push((key, size));
                                }
                                Ok(())
                            }
                        }
                    }
                },
            )
            .await;
        if let Err(error) = list_result {
            let _ = tx.send(Err(error)).await;
            if let Ok(mut guard) = reading_ahead.lock() {
                guard.clear();
            }
            return;
        }
        let files = match pending_files.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => Vec::new(),
        };
        for (key, size) in files {
            if let Ok(mut guard) = reading_ahead.lock() {
                *guard = key.clone();
            }
            let relative_path = PathBuf::from(&key);
            let candidate = ImportFileCandidate {
                path: relative_path.clone(),
                output_relative_path: Some(relative_path.clone()),
                relative_path,
                content: None,
                remote_root: Some(remote_root.clone()),
                size_hint: size,
            };
            let label = format!("import source {key}");
            match load_import_candidate_bytes(&candidate, max_input_bytes, &label).await {
                Ok(bytes) => {
                    if tx
                        .send(Ok(PipelineMsg::Loaded(PipelineLoadedSource {
                            candidate,
                            bytes,
                        })))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    break;
                }
            }
        }
        if let Ok(mut guard) = reading_ahead.lock() {
            guard.clear();
        }
    });
    (rx, producer)
}

async fn squash_storyline_into_store(
    store: &StorylineLanceStore,
    args: &ImportArgs,
    stdin: &mut dyn Read,
    progress: &mut ImportProgress,
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
    } = options;
    if args.stream {
        return squash_storyline_stdin_into_store(
            store,
            args.format,
            max_input_bytes,
            stdin,
            progress,
            seen_document_ids,
            duplicate_policy,
            allow_empty,
            directory_input,
            append_generation,
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
        max_input_bytes,
        progress,
        source,
        seen_document_ids,
        duplicate_policy,
        allow_empty,
        directory_input,
        append_generation,
        commit_batch_schedule(args),
    )
    .await
}

const DEFAULT_COMMIT_BATCH_START: usize = 64;
const DEFAULT_COMMIT_BATCH_MAX: usize = 4096;

#[derive(Debug, Clone)]
struct CommitBatchSchedule {
    next: usize,
    max: usize,
    fixed: bool,
}

impl CommitBatchSchedule {
    fn adaptive() -> Self {
        Self {
            next: DEFAULT_COMMIT_BATCH_START,
            max: DEFAULT_COMMIT_BATCH_MAX,
            fixed: false,
        }
    }

    fn fixed(n: usize) -> Self {
        let n = n.max(1);
        Self {
            next: n,
            max: n,
            fixed: true,
        }
    }

    fn current(&self) -> usize {
        self.next
    }

    fn after_commit(&mut self) {
        if self.fixed {
            return;
        }
        self.next = self.next.saturating_mul(2).min(self.max);
    }
}

fn commit_batch_schedule(args: &ImportArgs) -> CommitBatchSchedule {
    match args.commit_every {
        Some(n) => CommitBatchSchedule::fixed(n),
        None => CommitBatchSchedule::adaptive(),
    }
}

enum ObjectStoreImportSource {
    Candidates(Vec<ImportFileCandidate>),
    Location(DatasetLocation),
}

#[allow(clippy::too_many_arguments)]
async fn squash_storyline_files_pipeline(
    store: &StorylineLanceStore,
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    progress: &mut ImportProgress,
    source: ObjectStoreImportSource,
    mut seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    directory_input: bool,
    mut append_generation: Option<String>,
    mut commit_schedule: CommitBatchSchedule,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let reading_ahead = Arc::new(std::sync::Mutex::new(String::new()));
    let (mut rx, producer) = match source {
        ObjectStoreImportSource::Candidates(candidates) => {
            spawn_candidates_load_producer(candidates, max_input_bytes, Arc::clone(&reading_ahead))
        }
        ObjectStoreImportSource::Location(location) => {
            progress.set_phase(ImportPhase::Discovering, location.as_str())?;
            spawn_object_store_discover_load_producer(
                location,
                max_input_bytes,
                Arc::clone(&reading_ahead),
            )
        }
    };

    let mut unknown_field_warnings =
        persisting_pchronicle::model::UnknownFieldImportWarnings::default();
    let mut skipped_warnings = Vec::new();
    let mut imported_sources: Vec<ImportedSource> = Vec::new();
    let mut batch = Vec::with_capacity(commit_schedule.current());
    let mut committed_storylines = 0u64;
    let mut skipped_commit_storylines = 0usize;
    let mut saw_any = false;
    let mut current_storylines = Vec::new().into_iter();
    let mut producer_done = false;
    let mut discovered_any = false;

    loop {
        if let Some(mut storyline) = current_storylines.next() {
            saw_any = true;
            if let Some(warning) =
                apply_duplicate_document_policy(&mut storyline, &mut seen_document_ids, duplicate_policy)
            {
                if warning.contains("skipped") {
                    skipped_warnings.push(warning);
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
            batch.push(storyline);
            if batch.len() >= commit_schedule.current() {
                match commit_or_skip_storyline_import_batch(
                    store,
                    progress,
                    std::mem::take(&mut batch),
                    &mut append_generation,
                    committed_storylines,
                    &mut commit_schedule,
                )
                .await?
                {
                    StorylineBatchCommit::Committed(total) => {
                        committed_storylines = total;
                    }
                    StorylineBatchCommit::Skipped { batch_len, warning } => {
                        skipped_commit_storylines = skipped_commit_storylines
                            .saturating_add(batch_len as usize);
                        skipped_warnings.push(warning);
                        retract_imported_trajectories(
                            &mut imported_sources,
                            batch_len as usize,
                        );
                    }
                }
                batch.reserve(commit_schedule.current());
            }
            continue;
        }

        if producer_done {
            break;
        }

        // Surface producer read activity while waiting for the next Source.
        let msg = loop {
            if let Ok(guard) = reading_ahead.lock() {
                progress.set_reading_ahead(guard.as_str())?;
            }
            tokio::select! {
                item = rx.recv() => break item,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
        };
        match msg {
            Some(Ok(PipelineMsg::Scanning(prefix))) => {
                progress.note_scanning(&prefix)?;
            }
            Some(Ok(PipelineMsg::Discovered { path, bytes })) => {
                discovered_any = true;
                progress.note_discovered(&path, bytes)?;
            }
            Some(Ok(PipelineMsg::Loaded(loaded))) => {
                let name = loaded.candidate.relative_path.to_string_lossy().into_owned();
                if let Ok(guard) = reading_ahead.lock() {
                    progress.set_reading_ahead(guard.as_str())?;
                }
                progress.set_phase(ImportPhase::Parsing, &name)?;
                match decode_import_source(
                    requested_format,
                    ImportOutputFormat::Storyline,
                    Some(&loaded.candidate.path),
                    Some(&loaded.candidate.relative_path),
                    loaded.candidate.output_relative_path.as_deref(),
                    &loaded.bytes,
                    &mut unknown_field_warnings,
                )? {
                    DecodeImportOutcome::Imported(decoded) => {
                        progress.set_phase(
                            ImportPhase::Writing,
                            &decoded.diagnostic_path.to_string_lossy(),
                        )?;
                        progress.note_imported(decoded.metadata.input_bytes as u64)?;
                        let mut metadata = decoded.metadata;
                        metadata.trajectories = 0;
                        imported_sources.push(metadata);
                        current_storylines = decoded.storylines.into_iter();
                    }
                    DecodeImportOutcome::Skipped { path, reason } => {
                        progress.note_imported(0)?;
                        skipped_warnings.push(skipped_import_warning(&path, &reason));
                    }
                }
            }
            Some(Err(error)) => {
                producer.abort();
                return Err(error);
            }
            None => {
                producer_done = true;
                progress.clear_reading_ahead()?;
            }
        }
    }

    if !batch.is_empty() {
        match commit_or_skip_storyline_import_batch(
            store,
            progress,
            std::mem::take(&mut batch),
            &mut append_generation,
            committed_storylines,
            &mut commit_schedule,
        )
        .await?
        {
            StorylineBatchCommit::Committed(total) => {
                committed_storylines = total;
            }
            StorylineBatchCommit::Skipped { batch_len, warning } => {
                skipped_commit_storylines =
                    skipped_commit_storylines.saturating_add(batch_len as usize);
                skipped_warnings.push(warning);
                retract_imported_trajectories(&mut imported_sources, batch_len as usize);
            }
        }
    }
    progress.clear_reading_ahead()?;

    match producer.await {
        Ok(()) => {}
        Err(error) if error.is_cancelled() => {}
        Err(error) => return Err(anyhow!("import reader task failed: {error}")),
    }

    if imported_sources.is_empty() {
        if allow_empty && !saw_any {
            return Ok((imported_sources, unknown_field_warnings, skipped_warnings));
        }
        if !discovered_any {
            return Err(cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "import object prefix contains no .json, .jsonl, or .ndjson files",
            ));
        }
        return Err(empty_auto_directory_import_error(directory_input));
    }
    // Drop Sources that lost every trajectory to skipped commits so empty
    // placeholders do not inflate the import summary.
    if skipped_commit_storylines > 0 {
        imported_sources.retain(|source| source.trajectories > 0);
    }
    if imported_sources.is_empty() {
        return Err(anyhow!(
            "storyline import committed no trajectories after skipping failed batches"
        ));
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
async fn squash_storyline_stdin_into_store(
    store: &StorylineLanceStore,
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    stdin: &mut dyn Read,
    progress: &mut ImportProgress,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    allow_empty: bool,
    directory_input: bool,
    append_generation: Option<String>,
    commit_schedule: CommitBatchSchedule,
) -> Result<(
    Vec<ImportedSource>,
    persisting_pchronicle::model::UnknownFieldImportWarnings,
    Vec<String>,
)> {
    let import = StorylineImportIterator::stdin(
        requested_format,
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
    )
    .await
}

fn apply_duplicate_document_policy(
    storyline: &mut StorylineDocument,
    seen_document_ids: &mut HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
) -> Option<String> {
    let original = storyline.document_id().to_string();
    match duplicate_policy {
        DuplicateIdPolicy::Suffix => {
            uniquify_storyline_document_id(storyline, seen_document_ids).map(
                |(original, renamed)| {
                    format!("warning: duplicate document_id '{original}' renamed to '{renamed}'")
                },
            )
        }
        DuplicateIdPolicy::Skip => {
            if !seen_document_ids.insert(original.clone()) {
                Some(format!(
                    "warning: duplicate document_id '{original}' skipped"
                ))
            } else {
                None
            }
        }
    }
}

async fn drain_storyline_import_batches(
    store: &StorylineLanceStore,
    mut import: StorylineImportIterator<'_>,
    mut append_generation: Option<String>,
    mut commit_schedule: CommitBatchSchedule,
    allow_empty: bool,
    directory_input: bool,
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
                match commit_or_skip_storyline_import_batch(
                    store,
                    import.progress,
                    std::mem::take(&mut batch),
                    &mut append_generation,
                    committed_storylines,
                    &mut commit_schedule,
                )
                .await?
                {
                    StorylineBatchCommit::Committed(total) => {
                        committed_storylines = total;
                    }
                    StorylineBatchCommit::Skipped { batch_len, warning } => {
                        skipped_commit_storylines = skipped_commit_storylines
                            .saturating_add(batch_len as usize);
                        commit_skip_warnings.push(warning);
                    }
                }
                batch.reserve(commit_schedule.current());
            }
            None if batch.is_empty() => break,
            None => {
                match commit_or_skip_storyline_import_batch(
                    store,
                    import.progress,
                    std::mem::take(&mut batch),
                    &mut append_generation,
                    committed_storylines,
                    &mut commit_schedule,
                )
                .await?
                {
                    StorylineBatchCommit::Committed(total) => {
                        committed_storylines = total;
                    }
                    StorylineBatchCommit::Skipped { batch_len, warning } => {
                        skipped_commit_storylines = skipped_commit_storylines
                            .saturating_add(batch_len as usize);
                        commit_skip_warnings.push(warning);
                    }
                }
                break;
            }
        }
    }

    let (mut imported_sources, unknown_field_warnings, mut skipped_warnings) =
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
    finalize_storyline_import_indexes(store, import.progress).await?;
    Ok((imported_sources, unknown_field_warnings, skipped_warnings))
}

enum StorylineBatchCommit {
    Committed(u64),
    Skipped { batch_len: u64, warning: String },
}

fn is_skippable_storyline_commit_error(error: &anyhow::Error) -> bool {
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
}

fn retract_imported_trajectories(sources: &mut [ImportedSource], mut count: usize) {
    for source in sources.iter_mut().rev() {
        if count == 0 {
            break;
        }
        let take = source.trajectories.min(count);
        source.trajectories -= take;
        count -= take;
    }
}

async fn refresh_append_generation_after_skip(
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

async fn commit_or_skip_storyline_import_batch(
    store: &StorylineLanceStore,
    progress: &mut ImportProgress,
    batch: Vec<StorylineDocument>,
    append_generation: &mut Option<String>,
    committed_storylines: u64,
    commit_schedule: &mut CommitBatchSchedule,
) -> Result<StorylineBatchCommit> {
    let batch_len = batch.len() as u64;
    let sample_ids = batch
        .iter()
        .take(8)
        .map(|storyline| storyline.document_id().to_string())
        .collect::<Vec<_>>();
    match commit_storyline_import_batch(
        store,
        progress,
        batch,
        append_generation,
        committed_storylines,
    )
    .await
    {
        Ok(total) => {
            commit_schedule.after_commit();
            Ok(StorylineBatchCommit::Committed(total))
        }
        Err(error) if is_skippable_storyline_commit_error(&error) => {
            tracing::warn!(
                committed_before = committed_storylines,
                batch_len,
                root = %store.root_uri(),
                sample_document_ids = ?sample_ids,
                error = %format!("{error:#}"),
                "skipping storyline commit batch after transient storage failure; continuing import"
            );
            refresh_append_generation_after_skip(store, append_generation).await;
            if !commit_schedule.fixed {
                commit_schedule.next = DEFAULT_COMMIT_BATCH_START;
            }
            let warning = format!(
                "warning: skipped storyline commit batch of {batch_len} trajectories (committed_before={committed_storylines}, sample_document_ids={sample_ids:?}): {error:#}"
            );
            let _ = progress.notice(&warning);
            Ok(StorylineBatchCommit::Skipped { batch_len, warning })
        }
        Err(error) => Err(error),
    }
}

async fn finalize_storyline_import_indexes(
    store: &StorylineLanceStore,
    progress: &mut ImportProgress,
) -> Result<()> {
    progress.set_phase(ImportPhase::Writing, "optimize indices (final)")?;
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
    progress.set_phase(ImportPhase::Writing, "optimize indices done")?;
    Ok(())
}

fn collect_local_relative_files(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("read staging directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("strip staging root from {}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            if !relative.is_empty() {
                out.push(relative);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn is_deferred_storyline_publish_key(relative: &str) -> bool {
    matches!(
        relative,
        "CURRENT" | "chronicle.manifest" | ".storyline-write.lock"
    ) || relative.ends_with("/CURRENT")
        || relative.ends_with("/chronicle.manifest")
}

async fn upload_local_storyline_dataset(
    local_root: &Path,
    destination: &DatasetLocation,
    progress: &mut ImportProgress,
) -> Result<()> {
    let files = collect_local_relative_files(local_root)?;
    anyhow::ensure!(
        files.iter().any(|path| path == "CURRENT"),
        "staged Storyline Dataset is missing CURRENT"
    );
    let (deferred, eager): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|path| is_deferred_storyline_publish_key(path));
    let total = eager.len().saturating_add(deferred.len()) as u64;
    let mut uploaded = 0u64;
    for relative in eager.into_iter().chain(deferred) {
        if relative == ".storyline-write.lock" {
            continue;
        }
        uploaded = uploaded.saturating_add(1);
        progress.set_phase(
            ImportPhase::Writing,
            &format!(
                "upload {uploaded}/{total} {}",
                truncate_middle(&relative, 56)
            ),
        )?;
        let bytes = tokio::fs::read(local_root.join(&relative))
            .await
            .with_context(|| format!("read staged file {relative}"))?;
        destination
            .write_relative_bytes(&relative, &bytes)
            .await
            .with_context(|| format!("upload staged file {relative}"))?;
    }
    progress.set_phase(ImportPhase::Writing, "upload complete")?;
    Ok(())
}

async fn commit_storyline_import_batch(
    store: &StorylineLanceStore,
    progress: &mut ImportProgress,
    batch: Vec<StorylineDocument>,
    append_generation: &mut Option<String>,
    committed_storylines: u64,
) -> Result<u64> {
    anyhow::ensure!(!batch.is_empty(), "storyline import commit batch is empty");
    let batch_len = batch.len() as u64;
    progress.set_phase(
        ImportPhase::Writing,
        &format!("commit {batch_len} trajectories"),
    )?;
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
    let total = committed_storylines
        .checked_add(batch_len)
        .context("import trajectory count overflow")?;
    persisting_pchronicle::storage::write_storyline_manifest_at_uri(
        store.root_uri(),
        &paths.generation,
        total,
        0,
    )
    .await
    .context("write progressive chronicle.manifest after storyline commit")?;
    *append_generation = Some(paths.generation.clone());
    progress.note_committed(total)?;
    Ok(total)
}

async fn run_canonical_event_import(
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

pub(super) async fn run_export(
    mut args: ExportArgs,
    settings_override: Option<&Path>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.max_trajectories > 0,
        "--max-trajectories must be greater than zero"
    );
    anyhow::ensure!(
        args.max_output_bytes > 0,
        "--max-output-bytes must be greater than zero"
    );
    anyhow::ensure!(
        args.timeout_seconds > 0,
        "--timeout must be greater than zero"
    );
    args.stream = args.output == "-" || args.stream;
    anyhow::ensure!(
        args.output == "-" || !args.stream,
        "--stream requires --to -"
    );
    anyhow::ensure!(
        !(args.output == "-" && args.overwrite),
        "--overwrite cannot be used with stdout"
    );
    if let Some(source) = &args.source {
        validate_source_path(source)?;
    }
    if let Some(run_id) = &args.run_id {
        validate_find_id("--run-id", run_id)?;
    }
    if let Some(document_id) = &args.document_id {
        validate_find_id("--document-id", document_id)?;
    }
    if let Some(session_id) = &args.session_id {
        validate_find_id("--session-id", session_id)?;
    }
    if let Some(expression) = &args.r#where {
        anyhow::ensure!(!expression.trim().is_empty(), "--where must not be empty");
        anyhow::ensure!(
            expression.len() <= 16 * 1024,
            "--where exceeds the 16384-byte limit"
        );
    }

    let format = ExchangeFormat::from(args.format);
    let dataset = resolve_dataset_uri(args.from.as_deref(), settings_override)?;
    if args.output != "-" {
        args.output = expand_dataset_reference(&args.output, settings_override, false)?;
    }
    if format == ExchangeFormat::CompactJsonl {
        anyhow::ensure!(
            args.source.is_none()
                && args.run_id.is_none()
                && args.document_id.is_none()
                && args.session_id.is_none()
                && args.r#where.is_none(),
            "compact JSONL export does not support filters"
        );
        anyhow::ensure!(
            args.output != "-",
            "compact JSONL export requires a directory output"
        );
        anyhow::ensure!(
            args.overwrite || !Path::new(&args.output).exists(),
            "export output already exists; pass --overwrite"
        );
        let rows =
            persisting_pchronicle::storage::CompactJsonlStore::export_path(&dataset, &args.output)
                .await?;
        writeln!(
            stderr,
            "format=compact-jsonl rows={} output={}",
            rows, args.output
        )?;
        return Ok(());
    }
    let (_, dataset_uris, snapshot) =
        discover_query_snapshot(Some(&dataset), &[], args.max_files, args.max_entries).await?;
    let dataset_uri = dataset_uris
        .first()
        .cloned()
        .context("export Dataset URI missing after discovery")?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let deadline = Duration::from_secs(args.timeout_seconds);
    let export = tokio::time::timeout(
        deadline,
        export_from_snapshot(&args, format, &dataset_uri, snapshot.clone()),
    )
    .await
    .with_context(|| {
        format!(
            "Dataset export timed out after {} seconds",
            args.timeout_seconds
        )
    })??;
    ensure_export_trajectory_budget(export.trajectories, args.max_trajectories)?;
    ensure_output_byte_budget(export.bytes.len(), args.max_output_bytes, "encoded export")?;
    write_export_output(&args.output, &export.bytes, args.overwrite, stdout).await?;
    writeln!(
        stderr,
        "snapshot_id={} format={} trajectories={} output_bytes={} exact={}",
        snapshot_id,
        format.as_str(),
        export.trajectories,
        export.bytes.len(),
        export.exact,
    )
    .context("write pChronicle export metadata")?;
    Ok(())
}

struct EncodedExport {
    bytes: Vec<u8>,
    trajectories: usize,
    exact: bool,
}

async fn export_from_snapshot(
    args: &ExportArgs,
    format: ExchangeFormat,
    dataset_uri: &str,
    snapshot: Arc<DatasetCatalogSnapshot>,
) -> Result<EncodedExport> {
    if let Some(export) = exact_local_file_export(args, format, dataset_uri, &snapshot).await? {
        return Ok(export);
    }
    anyhow::ensure!(
        !args.strict,
        "strict export requires an unfiltered source file already stored in the requested format"
    );

    let sql = export_address_sql(args)?;
    let engine = snapshot.clone().query_engine(Default::default()).await?;
    let row_limit = args
        .max_trajectories
        .checked_add(1)
        .context("--max-trajectories is too large")?;
    let mut addresses = LimitedBuffer::new(args.max_output_bytes);
    let write_result = engine
        .write_query_jsonl_bounded(&sql, &mut addresses, Some(row_limit))
        .await;
    let address_bytes = match addresses.finish(write_result)? {
        QueryOutputBudgetOutcome::Complete(bytes) => bytes,
        QueryOutputBudgetOutcome::RowLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "export exceeds max_trajectories limit of {}",
                    args.max_trajectories
                ),
            ));
        }
        QueryOutputBudgetOutcome::ByteLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "export address selection exceeds max_output_bytes limit of {}",
                    args.max_output_bytes
                ),
            ));
        }
    };
    let mut addresses = address_bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).context("decode export run address"))
        .collect::<Result<Vec<ExportAddress>>>()?;
    ensure_export_trajectory_budget(addresses.len(), args.max_trajectories)?;
    anyhow::ensure!(!addresses.is_empty(), "export selection matched no runs");
    addresses.sort_by(|left, right| {
        (&left.source_path, &left.document_id, &left.session_id).cmp(&(
            &right.source_path,
            &right.document_id,
            &right.session_id,
        ))
    });
    let mut stories = Vec::with_capacity(addresses.len());
    let mut normalized_bytes = 0usize;
    for address in &addresses {
        let key = CatalogStorylineKey {
            dataset: DEFAULT_DATASET_NAME.into(),
            file: address.source_path.clone(),
            document_id: address.document_id.clone(),
            session_id: address.session_id.clone(),
        };
        let story = snapshot
            .load_storyline(&key)
            .await
            .with_context(|| {
                format!(
                    "load export run {}/{}",
                    address.source_path, address.session_id
                )
            })?
            .with_context(|| {
                format!(
                    "export run disappeared from snapshot: {}/{}",
                    address.source_path, address.session_id
                )
            })?;
        anyhow::ensure!(
            story.trajectory_id.as_deref().unwrap_or(&story.session_id) == address.document_id,
            "export run document ID changed within the snapshot"
        );
        anyhow::ensure!(
            story.run_id == address.run_id,
            "export run runtime ID changed within the snapshot"
        );
        normalized_bytes = normalized_bytes.saturating_add(serde_json::to_vec(&story)?.len());
        ensure_output_byte_budget(normalized_bytes, args.max_output_bytes, "normalized export")?;
        stories.push(story);
    }
    let bytes = encode_export(format, &stories)?;
    Ok(EncodedExport {
        bytes,
        trajectories: stories.len(),
        exact: false,
    })
}

async fn exact_local_file_export(
    args: &ExportArgs,
    format: ExchangeFormat,
    dataset_uri: &str,
    snapshot: &DatasetCatalogSnapshot,
) -> Result<Option<EncodedExport>> {
    if args.document_id.is_some()
        || args.run_id.is_some()
        || args.session_id.is_some()
        || args.r#where.is_some()
    {
        return Ok(None);
    }
    let Some(dataset) = snapshot.dataset(DEFAULT_DATASET_NAME) else {
        return Ok(None);
    };
    let sources = dataset
        .sources
        .iter()
        .filter(|source| source.status == CatalogSourceStatus::Ready)
        .filter(|source| {
            args.source
                .as_deref()
                .is_none_or(|selected| selected == source.file)
        })
        .collect::<Vec<_>>();
    if sources.len() != 1 || sources[0].kind != CatalogSourceKind::File {
        return Ok(None);
    }
    let root = Path::new(dataset_uri);
    if !root.is_dir() {
        return Ok(None);
    }
    let source_path = root.join(&sources[0].file);
    let source_path = std::fs::canonicalize(&source_path).context("canonicalize export Source")?;
    anyhow::ensure!(
        source_path.starts_with(root),
        "export Source resolves outside the local Dataset"
    );
    let input = std::fs::read(&source_path).context("read exact export Source")?;
    ensure_output_byte_budget(input.len(), args.max_output_bytes, "exact export")?;
    let text = std::str::from_utf8(&input).context("exact export Source must be UTF-8")?;
    let detected = detect_format(Some(&source_path), Some(text))?;
    if detected != exchange_document_format(format) {
        return Ok(None);
    }
    let trajectories = validate_import_source(format, &source_path).await?;
    anyhow::ensure!(
        sources[0].size_bytes == Some(input.len() as u64)
            && sources[0].snapshot_ref().as_deref() == Some(&local_file_snapshot_ref(&source_path)),
        "export Source changed after the Snapshot was created"
    );
    Ok(Some(EncodedExport {
        bytes: input,
        trajectories,
        exact: true,
    }))
}

fn ensure_export_trajectory_budget(trajectories: usize, max_trajectories: u64) -> Result<()> {
    if usize::try_from(max_trajectories).is_ok_and(|limit| trajectories > limit) {
        return Err(cli_boundary_error(
            BoundaryCode::ResourceExhausted,
            format!("export exceeds max_trajectories limit of {max_trajectories}"),
        ));
    }
    Ok(())
}

fn export_address_sql(args: &ExportArgs) -> Result<String> {
    let mut predicates = Vec::new();
    if let Some(source) = &args.source {
        predicates.push(format!("_file_ = {}", sql_string(source)));
    }
    if let Some(run_id) = &args.run_id {
        predicates.push(format!("run_id = {}", sql_string(run_id)));
    }
    if let Some(document_id) = &args.document_id {
        predicates.push(format!("document_id = {}", sql_string(document_id)));
    }
    if let Some(session_id) = &args.session_id {
        predicates.push(format!("session_id = {}", sql_string(session_id)));
    }
    if let Some(expression) = &args.r#where {
        predicates.push(format!("({expression})"));
    }
    let predicate = if predicates.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", predicates.join(" AND "))
    };
    let limit = args
        .max_trajectories
        .checked_add(1)
        .context("--max-trajectories is too large")?;
    Ok(format!(
        "SELECT _file_ AS source_path, document_id, run_id, session_id \
         FROM dataset.trajectories{predicate} \
         ORDER BY _file_, document_id, session_id LIMIT {limit}"
    ))
}

fn encode_export(format: ExchangeFormat, stories: &[StorylineDocument]) -> Result<Vec<u8>> {
    let value = match format {
        ExchangeFormat::Atif => encode_json_storylines(DocumentFormat::Atif, stories)?,
        ExchangeFormat::Actf => encode_json_storylines(DocumentFormat::Actf, stories)?,
        ExchangeFormat::OpenaiMessages => {
            encode_json_storylines(DocumentFormat::OpenaiMsg, stories)?
        }
        ExchangeFormat::Storyline => encode_json_storylines(DocumentFormat::Storyline, stories)?,
        ExchangeFormat::Codex | ExchangeFormat::ClaudeCode => {
            bail!("{format} is decode-only and cannot be exported")
        }
        ExchangeFormat::CompactJsonl | ExchangeFormat::Auto => {
            unreachable!("exchange export format was validated")
        }
    };
    let mut output = serde_json::to_vec_pretty(&value).context("encode export JSON")?;
    output.push(b'\n');
    Ok(output)
}

fn exchange_document_format(format: ExchangeFormat) -> Option<DocumentFormat> {
    match format {
        ExchangeFormat::Atif => Some(DocumentFormat::Atif),
        ExchangeFormat::Actf => Some(DocumentFormat::Actf),
        ExchangeFormat::OpenaiMessages => Some(DocumentFormat::OpenaiMsg),
        ExchangeFormat::Storyline => Some(DocumentFormat::Storyline),
        ExchangeFormat::Codex => Some(DocumentFormat::Codex),
        ExchangeFormat::ClaudeCode => Some(DocumentFormat::ClaudeCode),
        ExchangeFormat::CompactJsonl | ExchangeFormat::Auto => None,
    }
}

async fn write_export_output(
    output: &str,
    bytes: &[u8],
    overwrite: bool,
    stdout: &mut dyn Write,
) -> Result<()> {
    if output == "-" {
        stdout.write_all(bytes).context("write export stream")?;
        return Ok(());
    }
    DatasetLocation::parse(output)?
        .put_bytes(bytes, overwrite)
        .await
}

fn local_file_snapshot_ref(path: &Path) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(path.to_string_lossy().as_bytes());
    if let Ok(metadata) = std::fs::metadata(path) {
        hash.update(&metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hash.update(&duration.as_nanos().to_le_bytes());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            hash.update(&metadata.dev().to_le_bytes());
            hash.update(&metadata.ino().to_le_bytes());
        }
    }
    format!("local:{}", hash.finalize().to_hex())
}

#[derive(Debug, Clone)]
struct ImportFileCandidate {
    path: PathBuf,
    relative_path: PathBuf,
    output_relative_path: Option<PathBuf>,
    /// Prefetched bytes (tests / rare callers). Normal imports leave this empty
    /// and read local paths or object-store keys on demand.
    content: Option<Vec<u8>>,
    /// Object-store Dataset root URI; when set, bytes are fetched lazily.
    remote_root: Option<String>,
    /// Size from discovery (`stat` / object metadata) for progress totals.
    size_hint: u64,
}

#[derive(Debug)]
struct ImportedSource {
    source_path: String,
    format: DocumentFormat,
    trajectories: usize,
    input_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportPhase {
    Discovering,
    Deleting,
    Reading,
    Parsing,
    Writing,
}

impl ImportPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Discovering => "discovering",
            Self::Deleting => "deleting",
            Self::Reading => "reading",
            Self::Parsing => "parsing",
            Self::Writing => "writing",
        }
    }
}

/// Dense import progress: TTY paints three in-place lines; redirected stderr gets
/// one summary line per completed source (buffered, flushed at the end).
struct ImportProgress {
    tty: bool,
    discovered_files: u64,
    discovered_bytes: u64,
    imported_files: u64,
    imported_bytes: u64,
    /// Storyline trajectories successfully committed so far.
    committed: u64,
    /// Replace/drop delete progress (separate from discovery totals).
    deleted_files: u64,
    delete_total: u64,
    phase: ImportPhase,
    file: String,
    /// Producer side of the read→parse pipeline (empty when idle).
    reading_ahead: String,
    painted: bool,
    log_lines: Vec<String>,
    last_paint: Option<std::time::Instant>,
    /// Shared with index-build callbacks so Lance work updates line 3 in place.
    surface: Arc<std::sync::Mutex<ImportProgressSurface>>,
}

#[derive(Debug, Clone)]
struct ImportProgressSurface {
    tty: bool,
    painted: bool,
    deleting: bool,
    line1: String,
    line2: String,
    reading_ahead: String,
    phase: String,
    file: String,
}

impl ImportProgressSurface {
    fn paint_activity(&mut self, activity: &str) -> Result<()> {
        if !self.tty {
            return Ok(());
        }
        let file = if activity.is_empty() {
            if self.file.is_empty() {
                "-".to_owned()
            } else {
                truncate_middle(&self.file, 72)
            }
        } else {
            truncate_middle(activity, 96)
        };
        let line3 = if !self.reading_ahead.is_empty() && !activity.is_empty() {
            format!(
                "[reading] {} | [writing] {file}",
                truncate_middle(&self.reading_ahead, 40),
            )
        } else if !self.reading_ahead.is_empty() && self.phase == "reading" {
            format!(
                "[reading] {}",
                truncate_middle(&self.reading_ahead, 96)
            )
        } else if !self.reading_ahead.is_empty() {
            format!(
                "[reading] {} | [{}] {file}",
                truncate_middle(&self.reading_ahead, 40),
                self.phase,
            )
        } else {
            format!("[{}] {file}", if activity.is_empty() { self.phase.as_str() } else { "writing" })
        };

        let mut err = std::io::stderr();
        if self.painted {
            write!(err, "\x1b[2A").context("move import progress cursor")?;
        }
        if self.deleting {
            write!(err, "\r\x1b[2K{}\n\r\x1b[2K\n\r\x1b[2K{line3}", self.line1)
                .context("paint delete progress")?;
        } else {
            write!(
                err,
                "\r\x1b[2K{}\n\r\x1b[2K{}\n\r\x1b[2K{line3}",
                self.line1, self.line2
            )
            .context("paint import progress")?;
        }
        err.flush().context("flush import progress")?;
        self.painted = true;
        Ok(())
    }
}

impl ImportProgress {
    fn new(tty: bool) -> Self {
        Self {
            tty,
            discovered_files: 0,
            discovered_bytes: 0,
            imported_files: 0,
            imported_bytes: 0,
            committed: 0,
            deleted_files: 0,
            delete_total: 0,
            phase: ImportPhase::Discovering,
            file: String::new(),
            reading_ahead: String::new(),
            painted: false,
            log_lines: Vec::new(),
            last_paint: None,
            surface: Arc::new(std::sync::Mutex::new(ImportProgressSurface {
                tty,
                painted: false,
                deleting: false,
                line1: String::new(),
                line2: String::new(),
                reading_ahead: String::new(),
                phase: ImportPhase::Discovering.as_str().to_owned(),
                file: String::new(),
            })),
        }
    }

    fn attach_index_progress(&self) -> persisting_pchronicle::storage::IndexBuildProgressGuard {
        let surface = Arc::clone(&self.surface);
        persisting_pchronicle::storage::install_index_build_progress(Arc::new(move |message| {
            if let Ok(mut surface) = surface.lock() {
                let _ = surface.paint_activity(message);
            }
        }))
    }

    fn reset_import_counters(&mut self) {
        self.imported_files = 0;
        self.imported_bytes = 0;
        self.committed = 0;
        self.deleted_files = 0;
        self.delete_total = 0;
        self.reading_ahead.clear();
        self.file.clear();
    }

    fn set_discovered(&mut self, files: u64, bytes: u64) -> Result<()> {
        self.discovered_files = files;
        self.discovered_bytes = bytes;
        self.phase = ImportPhase::Discovering;
        self.file.clear();
        self.paint(false)
    }

    fn note_discovered(&mut self, file: &str, bytes: u64) -> Result<()> {
        self.discovered_files = self.discovered_files.saturating_add(1);
        self.discovered_bytes = self.discovered_bytes.saturating_add(bytes);
        self.phase = ImportPhase::Discovering;
        self.file = file.to_owned();
        // Throttle TTY paints during large listings so discovery stays responsive.
        let should_paint = !self.tty
            || self
                .last_paint
                .map(|at| at.elapsed() >= std::time::Duration::from_millis(100))
                .unwrap_or(true)
            || self.discovered_files == 1
            || self.discovered_files % 64 == 0;
        if should_paint {
            self.paint(true)?;
        }
        Ok(())
    }

    fn note_scanning(&mut self, prefix: &str) -> Result<()> {
        self.phase = ImportPhase::Discovering;
        self.file = if prefix.is_empty() {
            "/".to_owned()
        } else {
            format!("{prefix}/")
        };
        let should_paint = !self.tty
            || self
                .last_paint
                .map(|at| at.elapsed() >= std::time::Duration::from_millis(100))
                .unwrap_or(true);
        if should_paint {
            self.paint(true)?;
        }
        Ok(())
    }

    fn note_deleted(&mut self, deleted: u64, total: u64, path: &str) -> Result<()> {
        self.deleted_files = deleted;
        self.delete_total = total;
        self.phase = ImportPhase::Deleting;
        self.file = path.to_owned();
        if deleted == total {
            // Always emit a final summary line for non-TTY logs.
            return self.paint(false);
        }
        let should_paint = !self.tty
            || path.is_empty()
            || self
                .last_paint
                .map(|at| at.elapsed() >= std::time::Duration::from_millis(100))
                .unwrap_or(true)
            || deleted == 1
            || deleted % 64 == 0;
        if should_paint {
            self.paint(true)?;
        }
        Ok(())
    }

    fn set_phase(&mut self, phase: ImportPhase, file: &str) -> Result<()> {
        self.phase = phase;
        self.file = file.to_owned();
        self.paint(true)
    }

    fn set_reading_ahead(&mut self, file: &str) -> Result<()> {
        self.reading_ahead = file.to_owned();
        let should_paint = !self.tty
            || self
                .last_paint
                .map(|at| at.elapsed() >= std::time::Duration::from_millis(100))
                .unwrap_or(true);
        if should_paint {
            self.paint(true)?;
        }
        Ok(())
    }

    fn clear_reading_ahead(&mut self) -> Result<()> {
        if self.reading_ahead.is_empty() {
            return Ok(());
        }
        self.reading_ahead.clear();
        self.paint(true)
    }

    fn note_imported(&mut self, bytes: u64) -> Result<()> {
        self.imported_files = self.imported_files.saturating_add(1);
        self.imported_bytes = self.imported_bytes.saturating_add(bytes);
        self.paint(false)
    }

    fn note_committed(&mut self, committed: u64) -> Result<()> {
        self.committed = committed;
        self.phase = ImportPhase::Writing;
        self.file = format!("commit trajectories={committed}");
        self.paint(false)
    }

    fn finish(&mut self) -> Result<()> {
        if let Ok(surface) = self.surface.lock() {
            self.painted = surface.painted;
        }
        if self.tty && self.painted {
            let mut err = std::io::stderr();
            writeln!(err).context("finish import progress")?;
            err.flush().context("flush import progress")?;
            self.painted = false;
            if let Ok(mut surface) = self.surface.lock() {
                surface.painted = false;
            }
        }
        Ok(())
    }

    fn notice(&mut self, message: &str) -> Result<()> {
        self.finish()?;
        if self.tty {
            let mut err = std::io::stderr();
            writeln!(err, "{message}").context("write import notice")?;
            err.flush().context("flush import notice")?;
        } else {
            self.log_lines.push(message.to_owned());
        }
        Ok(())
    }

    fn flush_log(self, out: &mut dyn Write) -> Result<()> {
        for line in self.log_lines {
            writeln!(out, "{line}").context("flush import progress log")?;
        }
        Ok(())
    }

    fn paint(&mut self, phase_only: bool) -> Result<()> {
        if let Ok(surface) = self.surface.lock() {
            self.painted = surface.painted;
        }
        let deleting = self.phase == ImportPhase::Deleting;
        let line1 = if deleting {
            format!(
                "deleted:total = {}/{}",
                self.deleted_files, self.delete_total
            )
        } else {
            format!(
                "imported:discovered = {}/{}",
                self.imported_files, self.discovered_files
            )
        };
        let line2 = if deleting {
            String::new()
        } else {
            format!(
                "committed = {} ; size = {}:{}",
                self.committed,
                format_byte_count(self.imported_bytes),
                format_byte_count(self.discovered_bytes)
            )
        };
        let file = if self.file.is_empty() {
            "-".to_owned()
        } else {
            truncate_middle(&self.file, 72)
        };
        let line3 = if !self.reading_ahead.is_empty()
            && matches!(
                self.phase,
                ImportPhase::Parsing | ImportPhase::Writing
            )
        {
            format!(
                "[reading] {} | [{}] {file}",
                truncate_middle(&self.reading_ahead, 48),
                self.phase.as_str(),
            )
        } else if !self.reading_ahead.is_empty() && self.phase == ImportPhase::Reading {
            format!(
                "[reading] {}",
                truncate_middle(&self.reading_ahead, 96)
            )
        } else {
            format!("[{}] {file}", self.phase.as_str())
        };

        if let Ok(mut surface) = self.surface.lock() {
            surface.tty = self.tty;
            surface.deleting = deleting;
            surface.line1 = line1.clone();
            surface.line2 = line2.clone();
            surface.reading_ahead = self.reading_ahead.clone();
            surface.phase = self.phase.as_str().to_owned();
            surface.file = self.file.clone();
            surface.painted = self.painted;
        }

        if self.tty {
            let mut err = std::io::stderr();
            if self.painted {
                write!(err, "\x1b[2A").context("move import progress cursor")?;
            }
            if deleting {
                write!(err, "\r\x1b[2K{line1}\n\r\x1b[2K\n\r\x1b[2K{line3}")
                    .context("paint delete progress")?;
            } else {
                write!(err, "\r\x1b[2K{line1}\n\r\x1b[2K{line2}\n\r\x1b[2K{line3}")
                    .context("paint import progress")?;
            }
            err.flush().context("flush import progress")?;
            self.painted = true;
            if let Ok(mut surface) = self.surface.lock() {
                surface.painted = true;
            }
            self.last_paint = Some(std::time::Instant::now());
            return Ok(());
        }

        if phase_only {
            return Ok(());
        }
        if deleting {
            self.log_lines
                .push(format!("{line1}; {line3}"));
        } else {
            self.log_lines
                .push(format!("{line1}; {line2}; {line3}"));
        }
        Ok(())
    }
}

fn format_byte_count(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1}GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1}MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1}KiB", value / KIB)
    } else {
        format!("{bytes}B")
    }
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max_chars {
        return value.to_owned();
    }
    if max_chars <= 3 {
        return chars.into_iter().take(max_chars).collect();
    }
    let head = (max_chars - 1) / 2;
    let tail = max_chars - 1 - head;
    let mut out: String = chars.iter().take(head).collect();
    out.push('…');
    out.extend(chars.iter().skip(chars.len() - tail));
    out
}

#[cfg(test)]
mod import_progress_tests {
    use super::*;

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
        assert!(!is_skippable_storyline_commit_error(&anyhow!(
            "duplicate document_id policy rejected payload"
        )));
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

    #[test]
    fn format_byte_count_uses_binary_units() {
        assert_eq!(format_byte_count(512), "512B");
        assert_eq!(format_byte_count(1536), "1.5KiB");
        assert_eq!(format_byte_count(2 * 1024 * 1024), "2.0MiB");
    }

    #[test]
    fn non_tty_progress_emits_dense_completed_lines() {
        let mut progress = ImportProgress::new(false);
        progress.set_discovered(2, 300).unwrap();
        progress.set_phase(ImportPhase::Reading, "a/long.json").unwrap();
        progress.set_phase(ImportPhase::Parsing, "a/long.json").unwrap();
        progress.note_imported(100).unwrap();
        progress.set_phase(ImportPhase::Writing, "b.json").unwrap();
        progress.note_imported(200).unwrap();
        progress.note_committed(3).unwrap();
        let mut out = Vec::new();
        progress.flush_log(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("imported:discovered = 1/2"), "{text}");
        assert!(text.contains("imported:discovered = 2/2"), "{text}");
        assert!(text.contains("committed = 3"), "{text}");
        assert!(text.contains("size ="), "{text}");
        assert!(text.contains("[writing] commit trajectories=3") || text.contains("[writing] b.json") || text.contains("[parsing] a/long.json"), "{text}");
        assert!(!text.contains("status=fetching"), "{text}");
    }
}

fn collect_import_candidates(input: &Path) -> Result<(bool, Vec<ImportFileCandidate>)> {
    let metadata = std::fs::symlink_metadata(input)
        .with_context(|| format!("inspect import input {}", input.display()))?;
    let explicit_file = if metadata.file_type().is_symlink() {
        std::fs::metadata(input)
            .with_context(|| format!("inspect import input target {}", input.display()))?
            .is_file()
    } else {
        metadata.is_file()
    };
    if explicit_file {
        let relative_path = input
            .file_name()
            .map(PathBuf::from)
            .context("import input file has no filename")?;
        let size_hint = metadata.len();
        return Ok((
            false,
            vec![ImportFileCandidate {
                path: input.to_path_buf(),
                relative_path,
                output_relative_path: None,
                content: None,
                remote_root: None,
                size_hint,
            }],
        ));
    }
    anyhow::ensure!(
        metadata.is_dir(),
        "import input must be a regular file or directory"
    );

    let paths = collect_visible_json_files(input)?;
    let mut candidates = Vec::with_capacity(paths.len());
    for path in paths {
        let relative_path = path
            .strip_prefix(input)
            .context("derive Dataset-relative import source path")?
            .to_path_buf();
        let size_hint = std::fs::metadata(&path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        candidates.push(ImportFileCandidate {
            path,
            output_relative_path: Some(relative_path.clone()),
            relative_path,
            content: None,
            remote_root: None,
            size_hint,
        });
    }
    candidates.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if candidates.is_empty() {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            "import directory contains no .json, .jsonl, or .ndjson files",
        ));
    }
    Ok((true, candidates))
}

/// Recursively collect absolute paths of visible `.json` / `.jsonl` / `.ndjson`
/// files under `root`. Shared by `import` and `sync`; not Catalog Directory
/// discovery (which is one-level and skips loose files).
pub(crate) fn collect_visible_json_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(&directory)
            .with_context(|| format!("read directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && is_visible_json_file(&path) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative.split('/').any(|part| part == "_meta") {
                    continue;
                }
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_visible_json_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "jsonl" | "ndjson"
            )
        })
}

async fn load_import_candidate_bytes(
    candidate: &ImportFileCandidate,
    max_input_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    if let Some(content) = &candidate.content {
        anyhow::ensure!(
            content.len() <= max_input_bytes,
            "{label} exceeds max_input_bytes limit of {max_input_bytes}"
        );
        return Ok(content.clone());
    }
    if let Some(remote_root) = &candidate.remote_root {
        let key = candidate.relative_path.to_string_lossy().replace('\\', "/");
        let location = DatasetLocation::parse(remote_root)?;
        let bytes = location
            .read_relative_bytes(&key)
            .await
            .with_context(|| format!("read import object {key} under {remote_root}"))?;
        anyhow::ensure!(
            bytes.len() <= max_input_bytes,
            "{label} exceeds max_input_bytes limit of {max_input_bytes}"
        );
        return Ok(bytes);
    }
    let file = std::fs::File::open(&candidate.path).with_context(|| format!("open {label}"))?;
    read_bounded(file, max_input_bytes, label)
}

fn scope_import_source_error(error: anyhow::Error, source_path: &Path) -> anyhow::Error {
    if let Some(boundary) = error.downcast_ref::<CliBoundaryError>() {
        return cli_boundary_error(
            boundary.code,
            format!("{}: {}", source_path.display(), boundary.message),
        );
    }
    error.context(format!("import source {}", source_path.display()))
}

struct DecodedImportSource {
    diagnostic_path: PathBuf,
    metadata: ImportedSource,
    storylines: Vec<StorylineDocument>,
}

enum DecodeImportOutcome {
    Imported(DecodedImportSource),
    Skipped { path: PathBuf, reason: String },
}

enum ImportFormatResolution {
    Format(ExchangeFormat),
    Skip(String),
}

enum StorylineImportInputs<'a> {
    Stdin(Option<&'a mut dyn Read>),
}

struct StorylineImportIterator<'a> {
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    progress: &'a mut ImportProgress,
    inputs: StorylineImportInputs<'a>,
    current: std::vec::IntoIter<StorylineDocument>,
    imported_sources: Vec<ImportedSource>,
    unknown_field_warnings: persisting_pchronicle::model::UnknownFieldImportWarnings,
    skipped_warnings: Vec<String>,
    seen_document_ids: HashSet<String>,
    duplicate_policy: DuplicateIdPolicy,
    failed: bool,
}

impl<'a> StorylineImportIterator<'a> {
    fn stdin(
        requested_format: ExchangeFormat,
        max_input_bytes: usize,
        stdin: &'a mut dyn Read,
        progress: &'a mut ImportProgress,
        seen_document_ids: HashSet<String>,
        duplicate_policy: DuplicateIdPolicy,
    ) -> Self {
        Self {
            requested_format,
            max_input_bytes,
            progress,
            inputs: StorylineImportInputs::Stdin(Some(stdin)),
            current: Vec::new().into_iter(),
            imported_sources: Vec::new(),
            unknown_field_warnings:
                persisting_pchronicle::model::UnknownFieldImportWarnings::default(),
            skipped_warnings: Vec::new(),
            seen_document_ids,
            duplicate_policy,
            failed: false,
        }
    }

    async fn decode_next_source(&mut self) -> Result<Option<DecodedImportSource>> {
        loop {
            let outcome = match &mut self.inputs {
                StorylineImportInputs::Stdin(stdin) => {
                    let Some(stdin) = stdin.take() else {
                        return Ok(None);
                    };
                    self.progress.set_phase(ImportPhase::Reading, "stdin")?;
                    let input = read_bounded(stdin, self.max_input_bytes, "stdin")?;
                    self.progress.set_phase(ImportPhase::Parsing, "stdin")?;
                    decode_import_source(
                        self.requested_format,
                        ImportOutputFormat::Storyline,
                        None,
                        None,
                        None,
                        &input,
                        &mut self.unknown_field_warnings,
                    )?
                }
            };
            match outcome {
                DecodeImportOutcome::Imported(decoded) => {
                    self.progress.set_phase(
                        ImportPhase::Writing,
                        &decoded.diagnostic_path.to_string_lossy(),
                    )?;
                    self.progress
                        .note_imported(decoded.metadata.input_bytes as u64)?;
                    return Ok(Some(decoded));
                }
                DecodeImportOutcome::Skipped { path, reason } => {
                    self.progress.note_imported(0)?;
                    self.skipped_warnings
                        .push(skipped_import_warning(&path, &reason));
                }
            }
        }
    }

    fn into_result_parts(
        self,
    ) -> (
        Vec<ImportedSource>,
        persisting_pchronicle::model::UnknownFieldImportWarnings,
        Vec<String>,
    ) {
        (
            self.imported_sources,
            self.unknown_field_warnings,
            self.skipped_warnings,
        )
    }

    async fn next_document(&mut self) -> Option<Result<StorylineDocument>> {
        loop {
            if let Some(mut storyline) = self.current.next() {
                let original = storyline.document_id().to_string();
                match self.duplicate_policy {
                    DuplicateIdPolicy::Suffix => {
                        if let Some((original, renamed)) = uniquify_storyline_document_id(
                            &mut storyline,
                            &mut self.seen_document_ids,
                        ) {
                            self.skipped_warnings.push(format!(
                                "warning: duplicate document_id '{original}' renamed to '{renamed}'"
                            ));
                        }
                    }
                    DuplicateIdPolicy::Skip => {
                        if !self.seen_document_ids.insert(original.clone()) {
                            self.skipped_warnings.push(format!(
                                "warning: duplicate document_id '{original}' skipped"
                            ));
                            continue;
                        }
                    }
                }
                let metadata = self
                    .imported_sources
                    .last_mut()
                    .expect("decoded Storyline has source metadata");
                metadata.trajectories = metadata
                    .trajectories
                    .checked_add(1)
                    .expect("import trajectory count overflow");
                return Some(Ok(storyline));
            }
            if self.failed {
                return None;
            }
            match self.decode_next_source().await {
                Ok(Some(decoded)) => {
                    let mut metadata = decoded.metadata;
                    metadata.trajectories = 0;
                    self.imported_sources.push(metadata);
                    self.current = decoded.storylines.into_iter();
                }
                Ok(None) => return None,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

fn uniquify_storyline_document_id(
    story: &mut StorylineDocument,
    seen: &mut HashSet<String>,
) -> Option<(String, String)> {
    let preferred = story.document_id().to_string();
    if seen.insert(preferred.clone()) {
        return None;
    }
    let mut suffix = 1u64;
    let renamed = loop {
        let candidate = format!("{preferred}#{suffix}");
        if seen.insert(candidate.clone()) {
            break candidate;
        }
        suffix = suffix
            .checked_add(1)
            .expect("document_id disambiguation suffix overflow");
    };
    if story
        .trajectory_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
    {
        story.trajectory_id = Some(renamed.clone());
    } else {
        story.session_id = renamed.clone();
    }
    Some((preferred, renamed))
}

#[allow(clippy::too_many_arguments)]
fn decode_import_source(
    requested_format: ExchangeFormat,
    output_format: ImportOutputFormat,
    input_path: Option<&Path>,
    decode_relative_path: Option<&Path>,
    logical_source_path: Option<&Path>,
    input: &[u8],
    unknown_field_warnings: &mut persisting_pchronicle::model::UnknownFieldImportWarnings,
) -> Result<DecodeImportOutcome> {
    let diagnostic_path = decode_relative_path
        .unwrap_or_else(|| Path::new("stdin"))
        .to_path_buf();
    let text = std::str::from_utf8(input).map_err(|error| {
        cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{} is not UTF-8: {error}", diagnostic_path.display()),
        )
    })?;
    let allow_skip = requested_format == ExchangeFormat::Auto && logical_source_path.is_some();
    let format = match resolve_import_format(requested_format, input_path, text, allow_skip)
        .map_err(|error| {
            if logical_source_path.is_some() {
                scope_import_source_error(error, &diagnostic_path)
            } else {
                error
            }
        })? {
        ImportFormatResolution::Format(format) => format,
        ImportFormatResolution::Skip(reason) => {
            return Ok(DecodeImportOutcome::Skipped {
                path: diagnostic_path,
                reason,
            });
        }
    };
    let document_format = exchange_document_format(format)
        .context("supported import format must map to a physical document format")?;
    let source_path = logical_source_path
        .map(PathBuf::from)
        .unwrap_or_else(|| single_import_source_path(format, output_format, input_path));
    let decode_relative_path = decode_relative_path.unwrap_or(&source_path);
    let storylines =
        decode_json_storylines(document_format, text, decode_relative_path).map_err(|issue| {
            let code = match issue.kind() {
                InputIssueKind::Invalid => BoundaryCode::InvalidRequest,
                InputIssueKind::Unsupported => BoundaryCode::Unsupported,
            };
            cli_boundary_error(
                code,
                import_input_issue_message(&issue, decode_relative_path),
            )
        });
    let storylines = match storylines {
        Ok(storylines) => storylines,
        Err(error) if allow_skip => {
            return Ok(DecodeImportOutcome::Skipped {
                path: diagnostic_path,
                reason: error.to_string(),
            });
        }
        Err(error) => return Err(error),
    };
    unknown_field_warnings
        .observe_storylines(&storylines)
        .map_err(|issue| {
            cli_boundary_error(
                BoundaryCode::InvalidRequest,
                import_input_issue_message(&issue, decode_relative_path),
            )
        })?;

    let metadata = ImportedSource {
        source_path: source_path
            .to_str()
            .context("Dataset-relative import Source path is not UTF-8")?
            .to_owned(),
        format: document_format,
        trajectories: storylines.len(),
        input_bytes: input.len(),
    };
    Ok(DecodeImportOutcome::Imported(DecodedImportSource {
        diagnostic_path,
        metadata,
        storylines,
    }))
}

#[allow(clippy::too_many_arguments)]
fn stage_preserved_import_source(
    requested_format: ExchangeFormat,
    input_path: Option<&Path>,
    decode_relative_path: Option<&Path>,
    logical_source_path: Option<&Path>,
    input: &[u8],
    staging_root: &Path,
    unknown_field_warnings: &mut persisting_pchronicle::model::UnknownFieldImportWarnings,
    skipped_warnings: &mut Vec<String>,
) -> Result<Option<ImportedSource>> {
    let decoded = match decode_import_source(
        requested_format,
        ImportOutputFormat::Preserve,
        input_path,
        decode_relative_path,
        logical_source_path,
        input,
        unknown_field_warnings,
    )? {
        DecodeImportOutcome::Imported(decoded) => decoded,
        DecodeImportOutcome::Skipped { path, reason } => {
            skipped_warnings.push(skipped_import_warning(&path, &reason));
            return Ok(None);
        }
    };
    validate_import_storylines(&decoded.storylines).map_err(|error| {
        if logical_source_path.is_some() {
            scope_import_source_error(error, &decoded.diagnostic_path)
        } else {
            error
        }
    })?;

    let staged_source = staging_root.join(&decoded.metadata.source_path);
    let staged_parent = staged_source
        .parent()
        .context("staged import Source has no parent")?;
    std::fs::create_dir_all(staged_parent)
        .with_context(|| format!("create staged Source parent {}", staged_parent.display()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_source)
        .with_context(|| format!("create staged Source {}", decoded.metadata.source_path))?;
    file.write_all(input)
        .with_context(|| format!("write staged Source {}", decoded.metadata.source_path))?;
    file.sync_all()
        .with_context(|| format!("sync staged Source {}", decoded.metadata.source_path))?;
    Ok(Some(decoded.metadata))
}

fn read_bounded(mut reader: impl Read, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    if max_bytes == usize::MAX {
        reader
            .read_to_end(&mut input)
            .with_context(|| format!("read {label}"))?;
    } else {
        let limit = u64::try_from(max_bytes)
            .ok()
            .and_then(|limit| limit.checked_add(1))
            .ok_or_else(|| {
                cli_boundary_error(
                    BoundaryCode::InvalidRequest,
                    "--max-input-bytes is too large",
                )
            })?;
        reader
            .by_ref()
            .take(limit)
            .read_to_end(&mut input)
            .with_context(|| format!("read {label}"))?;
        if input.len() > max_bytes {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!("{label} exceeds max_input_bytes limit of {max_bytes}"),
            ));
        }
    }
    if input.is_empty() {
        return Err(cli_boundary_error(
            BoundaryCode::InvalidRequest,
            format!("{label} is empty"),
        ));
    }
    Ok(input)
}

fn resolve_import_format(
    requested: ExchangeFormat,
    input_path: Option<&Path>,
    input: &str,
    allow_skip: bool,
) -> Result<ImportFormatResolution> {
    let format = match requested {
        ExchangeFormat::Auto => match detect_format(input_path, Some(input))? {
            Some(DocumentFormat::Atif) => ExchangeFormat::Atif,
            Some(DocumentFormat::Actf) => ExchangeFormat::Actf,
            Some(DocumentFormat::OpenaiMsg) => ExchangeFormat::OpenaiMessages,
            Some(DocumentFormat::Storyline) => ExchangeFormat::Storyline,
            Some(DocumentFormat::Codex) => ExchangeFormat::Codex,
            Some(DocumentFormat::ClaudeCode) => ExchangeFormat::ClaudeCode,
            Some(format) if allow_skip => {
                return Ok(ImportFormatResolution::Skip(format!(
                    "detected import format '{format}' is not a queryable JSON format"
                )));
            }
            Some(format) => {
                return Err(cli_boundary_error(
                    BoundaryCode::Unsupported,
                    format!("detected import format '{format}' is not a queryable JSON format"),
                ));
            }
            None if allow_skip && looks_like_json_document(input) => {
                return Ok(ImportFormatResolution::Skip(
                    "cannot detect import format".into(),
                ));
            }
            None => {
                return Err(cli_boundary_error(
                    BoundaryCode::InvalidRequest,
                    "cannot detect import format; pass --format explicitly",
                ));
            }
        },
        ExchangeFormat::Atif => ExchangeFormat::Atif,
        ExchangeFormat::Actf => ExchangeFormat::Actf,
        ExchangeFormat::OpenaiMessages => ExchangeFormat::OpenaiMessages,
        ExchangeFormat::Storyline => ExchangeFormat::Storyline,
        ExchangeFormat::Codex => ExchangeFormat::Codex,
        ExchangeFormat::ClaudeCode => ExchangeFormat::ClaudeCode,
        ExchangeFormat::CompactJsonl => ExchangeFormat::CompactJsonl,
    };
    if !matches!(
        format,
        ExchangeFormat::Atif
            | ExchangeFormat::Actf
            | ExchangeFormat::OpenaiMessages
            | ExchangeFormat::Storyline
            | ExchangeFormat::Codex
            | ExchangeFormat::ClaudeCode
            | ExchangeFormat::CompactJsonl
    ) {
        return Err(cli_boundary_error(
            BoundaryCode::Unsupported,
            format!(
                "import format '{format}' is not supported by the first queryable import increment"
            ),
        ));
    }
    Ok(ImportFormatResolution::Format(format))
}

fn looks_like_json_document(input: &str) -> bool {
    let trimmed = input.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return true;
    }
    trimmed
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
}

fn skipped_import_warning(path: &Path, reason: &str) -> String {
    format!(
        "warning: skipped import source {}: {reason}",
        path.display()
    )
}

fn empty_auto_directory_import_error(directory_input: bool) -> anyhow::Error {
    cli_boundary_error(
        BoundaryCode::InvalidRequest,
        if directory_input {
            "import directory contains no detectable trajectory files"
        } else {
            "cannot detect import format; pass --format explicitly"
        },
    )
}

fn import_source_name(format: ExchangeFormat) -> &'static str {
    match format {
        ExchangeFormat::Atif => "trajectories.atif.json",
        ExchangeFormat::Actf => "trajectories.actf.json",
        ExchangeFormat::OpenaiMessages => "session_steps.json",
        ExchangeFormat::Storyline => "trajectories.storyline.json",
        ExchangeFormat::Codex => "session.codex.jsonl",
        ExchangeFormat::ClaudeCode => "session.claude-code.jsonl",
        ExchangeFormat::CompactJsonl => "compact.jsonl",
        _ => unreachable!("unsupported import format was rejected"),
    }
}

fn single_import_source_path(
    format: ExchangeFormat,
    output_format: ImportOutputFormat,
    input_path: Option<&Path>,
) -> PathBuf {
    if format == ExchangeFormat::Atif && output_format == ImportOutputFormat::Preserve {
        let line_extension = input_path
            .and_then(Path::extension)
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .filter(|extension| matches!(extension.as_str(), "jsonl" | "ndjson"));
        if let Some(extension) = line_extension {
            return PathBuf::from(format!("trajectories.atif.{extension}"));
        }
    }
    PathBuf::from(import_source_name(format))
}

fn import_input_issue_message(issue: &InputIssue, source_path: &Path) -> String {
    match issue.location() {
        Some(location) => format!("{} {location}: {}", source_path.display(), issue.message()),
        None => format!("{}: {}", source_path.display(), issue.message()),
    }
}

fn validate_import_storylines(storylines: &[StorylineDocument]) -> Result<usize> {
    Ok(storylines.len())
}

pub(super) async fn validate_import_source(format: ExchangeFormat, path: &Path) -> Result<usize> {
    let format = exchange_document_format(format)
        .context("supported import format must map to a physical document format")?;
    let source = open_document(format, path).await?;
    let mut seen = HashSet::new();
    let mut document_count = 0usize;
    source
        .for_each_storyline(|story| {
            let document_id = story.document_id();
            if !seen.insert(document_id.to_string()) {
                return Err(cli_boundary_error(
                    BoundaryCode::InvalidRequest,
                    "import contains duplicate document_id",
                ));
            }
            document_count = document_count
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("import document count overflow"))?;
            Ok(())
        })
        .await?;
    Ok(document_count)
}

struct StagingPathGuard {
    path: Option<PathBuf>,
}

impl StagingPathGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for StagingPathGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

async fn publish_staged_dataset(
    staging: &Path,
    output: &Path,
    replace_existing: bool,
    progress: Option<&mut ImportProgress>,
) -> Result<()> {
    let parent = output
        .parent()
        .context("Dataset output must have a parent directory")?;
    if !replace_existing {
        rename_noreplace(staging, output)
            .with_context(|| format!("publish new Dataset {}", output.display()))?;
        sync_dataset_parent(parent)?;
        return Ok(());
    }

    let backup = parent.join(format!(
        ".pchronicle-replace-{}-{}",
        output
            .file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_else(|| std::borrow::Cow::Borrowed("dataset")),
        uuid::Uuid::new_v4().simple()
    ));
    rename_noreplace(output, &backup)
        .with_context(|| format!("move existing Dataset to {}", backup.display()))?;
    if let Err(error) = sync_dataset_parent(parent) {
        return Err(rollback_replacement(output, &backup, error));
    }
    if let Err(error) = rename_noreplace(staging, output)
        .with_context(|| format!("publish replacement Dataset {}", output.display()))
    {
        return Err(rollback_replacement(output, &backup, error));
    }
    sync_dataset_parent(parent).with_context(|| {
        format!(
            "sync replacement Dataset parent {}; old Dataset remains at {}",
            parent.display(),
            backup.display()
        )
    })?;
    let backup_location = DatasetLocation::parse(
        backup
            .to_str()
            .context("replaced Dataset backup path is not valid UTF-8")?,
    )?;
    if let Some(progress) = progress {
        backup_location
            .remove_all_with_progress(|deleted, total, path| {
                progress.note_deleted(deleted, total, path)
            })
            .await
            .with_context(|| format!("delete replaced Dataset backup {}", backup.display()))?;
        progress.finish()?;
    } else {
        backup_location
            .remove_all()
            .await
            .with_context(|| format!("delete replaced Dataset backup {}", backup.display()))?;
    }
    sync_dataset_parent(parent)?;
    Ok(())
}

fn rollback_replacement(output: &Path, backup: &Path, error: anyhow::Error) -> anyhow::Error {
    match rename_noreplace(backup, output) {
        Ok(()) => error,
        Err(rollback_error) => anyhow!(
            "{error}; failed to restore old Dataset from {} to {}: {rollback_error}",
            backup.display(),
            output.display()
        ),
    }
}

fn sync_dataset_parent(parent: &Path) -> Result<()> {
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync Dataset parent {}", parent.display()))?;
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: both pointers come from live CString values and are NUL-terminated.
    // Call SYS_renameat2 directly so the binary still links on manylinux2014
    // (glibc 2.17). The renameat2() wrapper only exists in glibc 2.28+.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: both pointers come from live CString values and are NUL-terminated.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn rename_noreplace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic create-only Dataset publish is unsupported on this platform",
    ))
}
