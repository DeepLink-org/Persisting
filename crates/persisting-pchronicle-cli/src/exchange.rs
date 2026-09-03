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
    match args.mode {
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
            anyhow::ensure!(
                !parsed.is_object_store(),
                "replace mode for an existing object-store Dataset is unsupported; use a new URI"
            );
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
    anyhow::ensure!(
        args.mode == ImportMode::Append || args.on_duplicate.is_none(),
        "--on-duplicate is only valid with --mode append"
    );
    anyhow::ensure!(
        args.mode == ImportMode::Replace || !args.yes,
        "--yes is only valid with --mode replace"
    );
    anyhow::ensure!(
        !(args.stream && args.mode == ImportMode::Replace && !args.yes),
        "stdin replace import requires --yes because stdin carries the import data"
    );
    if args.from != "-" {
        args.from = expand_dataset_reference(&args.from, settings_override, true)?;
    }
    writeln!(stderr, "import from={} status=started", args.from)
        .context("write pChronicle import progress")?;
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
    let requested_destination = DatasetLocation::parse(&output_arg)?;
    if canonical.is_none()
        && requested_destination.is_object_store()
        && args.output_format != Some(ImportOutputFormat::Storyline)
    {
        anyhow::ensure!(
            args.mode == ImportMode::Append && args.output_format.is_none(),
            "object-store import requires --output-format storyline"
        );
    }
    let prepared =
        prepare_import_destination(&args, &output_arg, stdin_is_terminal, stdin, stderr).await?;
    let destination = prepared.location;
    let replace_existing = prepared.replace_existing;
    if let Some(snapshot) = canonical {
        anyhow::ensure!(
            args.mode != ImportMode::Append,
            "canonical event import does not support --mode append"
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
    let input_path = (!args.stream).then(|| Path::new(&args.from));
    let (directory_input, candidates) = if let Some(input_path) = input_path {
        collect_import_candidates(input_path)?
    } else {
        (false, Vec::new())
    };
    anyhow::ensure!(
        args.mode != ImportMode::Append || args.output_format != Some(ImportOutputFormat::Preserve),
        "append import requires --output-format storyline (or omit it)"
    );
    let output_format = args
        .output_format
        .unwrap_or(if args.mode == ImportMode::Append {
            ImportOutputFormat::Storyline
        } else {
            ImportOutputFormat::Preserve
        });
    let duplicate_policy = args.on_duplicate.unwrap_or(DuplicateIdPolicy::Suffix);
    let (dataset_uri, imported_sources, unknown_field_warnings, skipped_warnings) = if args.mode
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
                stderr,
                &candidates,
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
    } else if destination.is_object_store() {
        if destination.exists().await? {
            return Err(cli_boundary_error(
                BoundaryCode::Conflict,
                "import output already exists",
            ));
        }
        let store = StorylineLanceStore::open_uri(destination.as_str())
            .await
            .context("create squashed Storyline Lance Dataset")?;
        let (imported_sources, unknown_field_warnings, skipped_warnings) =
            squash_storyline_into_store(
                &store,
                &args,
                stdin,
                stderr,
                &candidates,
                StorylineImportOptions::create(max_input_bytes, directory_input),
            )
            .await?;
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
                    write_import_progress(stderr, "stdin", "processing", None)?;
                    let input = read_bounded(stdin, max_input_bytes, "stdin")?;
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
                        write_import_progress(
                            stderr,
                            &source.source_path,
                            "completed",
                            Some((&source.format, source.trajectories, source.input_bytes)),
                        )?;
                        imported_sources.push(source);
                    } else {
                        write_import_progress(stderr, "stdin", "skipped", None)?;
                    }
                } else {
                    for candidate in &candidates {
                        let label = format!("import source {}", candidate.relative_path.display());
                        write_import_progress(
                            stderr,
                            &candidate.relative_path.to_string_lossy(),
                            "processing",
                            None,
                        )?;
                        let file = std::fs::File::open(&candidate.path)
                            .with_context(|| format!("open {label}"))?;
                        let input = read_bounded(file, max_input_bytes, &label)?;
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
                            write_import_progress(
                                stderr,
                                &source.source_path,
                                "completed",
                                Some((&source.format, source.trajectories, source.input_bytes)),
                            )?;
                            imported_sources.push(source);
                        } else {
                            write_import_progress(
                                stderr,
                                &candidate.relative_path.to_string_lossy(),
                                "skipped",
                                None,
                            )?;
                        }
                    }
                }
                (imported_sources, unknown_field_warnings, skipped_warnings)
            }
            ImportOutputFormat::Storyline => {
                let store = StorylineLanceStore::open(staging.path())
                    .await
                    .context("create squashed Storyline Lance Dataset")?;
                squash_storyline_into_store(
                    &store,
                    &args,
                    stdin,
                    stderr,
                    &candidates,
                    StorylineImportOptions::create(max_input_bytes, directory_input),
                )
                .await?
            }
        };
        if imported_sources.is_empty() {
            return Err(empty_auto_directory_import_error(directory_input));
        }

        std::fs::File::open(staging.path())
            .and_then(|directory| directory.sync_all())
            .context("sync import staging directory")?;

        let staging_path = staging.keep();
        let mut cleanup = StagingPathGuard::new(staging_path.clone());
        publish_staged_dataset(&staging_path, &output, replace_existing)?;
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
    if let (Some(source_path), Some(format)) = (&response.source_path, &response.format) {
        writeln!(
            stderr,
            "dataset_uri={} source={} format={} output_format={} trajectories={} input_bytes={}",
            response.dataset_uri,
            source_path,
            format,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
        )
        .context("write pChronicle import metadata")?;
    } else {
        writeln!(
            stderr,
            "dataset_uri={} sources={} output_format={} trajectories={} input_bytes={}",
            response.dataset_uri,
            response.sources,
            response.output_format,
            response.trajectories,
            response
                .input_bytes
                .expect("JSON imports always report input bytes"),
        )
        .context("write pChronicle import metadata")?;
    }
    for line in skipped_warnings {
        writeln!(stderr, "{line}").context("write pChronicle skipped-source warning")?;
    }
    for line in unknown_field_warnings.warning_lines() {
        writeln!(stderr, "{line}").context("write pChronicle unknown-field warning")?;
    }
    Ok(())
}

/// Run one full snapshot import for the resident sync worker.
///
/// The existing import path already stages local outputs atomically, mirrors
/// deletions, and rebuilds a Storyline Lance destination from the same source
/// directory. Keeping the orchestration here avoids a second decoder or
/// Dataset publication protocol in the sync command.
pub(crate) async fn sync_snapshot(
    source: &Path,
    warehouse: &Path,
    storyline: &Path,
    input_format: ExchangeFormat,
) -> Result<()> {
    // ponytail: rebuild one atomic snapshot per coalesced batch; add affected-document mutation
    // when profiling shows full-directory rebuilds are the bottleneck.
    let mut stdout = std::io::sink();
    let mut stderr = std::io::sink();
    let mut stdin = std::io::empty();
    run_import(
        ImportArgs {
            from: source.to_string_lossy().into_owned(),
            output: Some(warehouse.to_string_lossy().into_owned()),
            format: input_format,
            output_format: Some(ImportOutputFormat::Preserve),
            mode: ImportMode::Replace,
            on_duplicate: None,
            yes: true,
            stream: false,
            max_input_bytes: Some(256 * 1024 * 1024),
        },
        None,
        false,
        &mut stdin,
        &mut stdout,
        &mut stderr,
    )
    .await
    .context("sync source into Warehouse")?;
    run_import(
        ImportArgs {
            from: source.to_string_lossy().into_owned(),
            output: Some(storyline.to_string_lossy().into_owned()),
            format: input_format,
            output_format: Some(ImportOutputFormat::Storyline),
            mode: ImportMode::Replace,
            on_duplicate: None,
            yes: true,
            stream: false,
            max_input_bytes: Some(256 * 1024 * 1024),
        },
        None,
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

async fn squash_storyline_into_store(
    store: &StorylineLanceStore,
    args: &ImportArgs,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
    candidates: &[ImportFileCandidate],
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
    let mut import = if args.stream {
        StorylineImportIterator::stdin(
            args.format,
            max_input_bytes,
            stdin,
            stderr,
            seen_document_ids,
            duplicate_policy,
        )
    } else {
        StorylineImportIterator::files(
            args.format,
            max_input_bytes,
            candidates,
            stderr,
            seen_document_ids,
            duplicate_policy,
        )
    };
    let report_storylines = match import.next() {
        Some(first) => match append_generation.as_deref() {
            Some(generation) => {
                store
                    .append_storyline_stream(std::iter::once(first).chain(&mut import), generation)
                    .await?
                    .storylines
            }
            None => {
                store
                    .replace_storyline_stream(std::iter::once(first).chain(&mut import))
                    .await?
                    .storylines
            }
        },
        None if allow_empty => 0,
        None => return Err(empty_auto_directory_import_error(directory_input)),
    };
    let (imported_sources, unknown_field_warnings, skipped_warnings) = import.into_result_parts();
    if imported_sources.is_empty() {
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
        report_storylines == imported_trajectories,
        "squashed Storyline import report does not match decoded trajectory count"
    );
    Ok((imported_sources, unknown_field_warnings, skipped_warnings))
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
        publish_staged_dataset(&staging_path, &output, true)?;
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
        ExchangeFormat::Auto => unreachable!("exchange export format was validated"),
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
        ExchangeFormat::Auto => None,
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

#[derive(Debug)]
struct ImportFileCandidate {
    path: PathBuf,
    relative_path: PathBuf,
    output_relative_path: Option<PathBuf>,
}

#[derive(Debug)]
struct ImportedSource {
    source_path: String,
    format: DocumentFormat,
    trajectories: usize,
    input_bytes: usize,
}

fn write_import_progress(
    stderr: &mut dyn Write,
    source: &str,
    status: &str,
    details: Option<(&DocumentFormat, usize, usize)>,
) -> Result<()> {
    if let Some((format, trajectories, input_bytes)) = details {
        writeln!(
            stderr,
            "import source={} status={} format={} trajectories={} input_bytes={}",
            source,
            status,
            format.as_str(),
            trajectories,
            input_bytes,
        )?;
    } else {
        writeln!(stderr, "import source={} status={}", source, status)?;
    }
    Ok(())
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
        return Ok((
            false,
            vec![ImportFileCandidate {
                path: input.to_path_buf(),
                relative_path,
                output_relative_path: None,
            }],
        ));
    }
    anyhow::ensure!(
        metadata.is_dir(),
        "import input must be a regular file or directory"
    );

    let mut pending = vec![input.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = pending.pop() {
        let mut entries = std::fs::read_dir(&directory)
            .with_context(|| format!("read import directory {}", directory.display()))?
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
            } else if file_type.is_file() && is_import_json_candidate(&path) {
                let relative_path = path
                    .strip_prefix(input)
                    .context("derive Dataset-relative import source path")?
                    .to_path_buf();
                candidates.push(ImportFileCandidate {
                    path,
                    output_relative_path: Some(relative_path.clone()),
                    relative_path,
                });
            }
        }
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

fn is_import_json_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "jsonl" | "ndjson"
            )
        })
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
    Files {
        candidates: &'a [ImportFileCandidate],
        next: usize,
    },
}

struct StorylineImportIterator<'a> {
    requested_format: ExchangeFormat,
    max_input_bytes: usize,
    progress: &'a mut dyn Write,
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
        progress: &'a mut dyn Write,
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

    fn files(
        requested_format: ExchangeFormat,
        max_input_bytes: usize,
        candidates: &'a [ImportFileCandidate],
        progress: &'a mut dyn Write,
        seen_document_ids: HashSet<String>,
        duplicate_policy: DuplicateIdPolicy,
    ) -> Self {
        Self {
            requested_format,
            max_input_bytes,
            progress,
            inputs: StorylineImportInputs::Files {
                candidates,
                next: 0,
            },
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

    fn decode_next_source(&mut self) -> Result<Option<DecodedImportSource>> {
        loop {
            let outcome = match &mut self.inputs {
                StorylineImportInputs::Stdin(stdin) => {
                    let Some(stdin) = stdin.take() else {
                        return Ok(None);
                    };
                    write_import_progress(self.progress, "stdin", "processing", None)?;
                    let input = read_bounded(stdin, self.max_input_bytes, "stdin")?;
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
                StorylineImportInputs::Files { candidates, next } => {
                    let Some(candidate) = candidates.get(*next) else {
                        return Ok(None);
                    };
                    *next = next
                        .checked_add(1)
                        .context("import Source index overflow")?;
                    let label = format!("import source {}", candidate.relative_path.display());
                    write_import_progress(
                        self.progress,
                        &candidate.relative_path.to_string_lossy(),
                        "processing",
                        None,
                    )?;
                    let file = std::fs::File::open(&candidate.path)
                        .with_context(|| format!("open {label}"))?;
                    let input = read_bounded(file, self.max_input_bytes, &label)?;
                    decode_import_source(
                        self.requested_format,
                        ImportOutputFormat::Storyline,
                        Some(&candidate.path),
                        Some(&candidate.relative_path),
                        candidate.output_relative_path.as_deref(),
                        &input,
                        &mut self.unknown_field_warnings,
                    )?
                }
            };
            match outcome {
                DecodeImportOutcome::Imported(decoded) => {
                    write_import_progress(
                        self.progress,
                        &decoded.diagnostic_path.to_string_lossy(),
                        "completed",
                        Some((
                            &decoded.metadata.format,
                            decoded.metadata.trajectories,
                            decoded.metadata.input_bytes,
                        )),
                    )?;
                    return Ok(Some(decoded));
                }
                DecodeImportOutcome::Skipped { path, reason } => {
                    write_import_progress(self.progress, &path.to_string_lossy(), "skipped", None)?;
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
}

impl Iterator for StorylineImportIterator<'_> {
    type Item = Result<StorylineDocument>;

    fn next(&mut self) -> Option<Self::Item> {
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
            match self.decode_next_source() {
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
        })?;
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
    };
    if !matches!(
        format,
        ExchangeFormat::Atif
            | ExchangeFormat::Actf
            | ExchangeFormat::OpenaiMessages
            | ExchangeFormat::Storyline
            | ExchangeFormat::Codex
            | ExchangeFormat::ClaudeCode
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

fn publish_staged_dataset(staging: &Path, output: &Path, replace_existing: bool) -> Result<()> {
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
    std::fs::remove_dir_all(&backup)
        .with_context(|| format!("delete replaced Dataset backup {}", backup.display()))?;
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
