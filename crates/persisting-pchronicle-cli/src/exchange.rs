use super::*;

pub(super) async fn run_import(
    args: ImportArgs,
    settings_override: Option<&Path>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.max_input_bytes > 0,
        "--max-input-bytes must be greater than zero"
    );
    anyhow::ensure!(
        (args.from == "-") == args.stream,
        "--stream requires --from -, and --from - requires --stream"
    );
    if args.stream {
        anyhow::ensure!(
            args.format != ExchangeFormat::Auto,
            "stdin import requires an explicit --format"
        );
    }
    let output_arg = match args.output.as_deref() {
        Some(output) => output.to_owned(),
        None => default_import_output(&args, settings_override)?,
    };
    let output = validate_new_local_dataset_path(&output_arg)?;
    let input_path = (!args.stream).then(|| Path::new(&args.from));
    let input = if args.stream {
        read_bounded(stdin, args.max_input_bytes, "stdin")?
    } else {
        let input_path = input_path.expect("non-stream input path");
        anyhow::ensure!(
            input_path.is_file(),
            "import input must be one regular file"
        );
        let file = std::fs::File::open(input_path)
            .with_context(|| format!("open import input {}", input_path.display()))?;
        read_bounded(file, args.max_input_bytes, "import input")?
    };
    let text = std::str::from_utf8(&input).context("import input must be UTF-8")?;
    let format = resolve_import_format(args.format, input_path, text)?;
    let source_path = import_source_name(format);
    let relative_path = input_path
        .and_then(Path::file_name)
        .map(Path::new)
        .unwrap_or_else(|| Path::new(source_path));
    let storylines = decode_json_storylines(
        exchange_document_format(format)
            .context("supported import format must map to a physical document format")?,
        text,
        relative_path,
    )
    .map_err(cli_input_error)?;
    let trajectories = validate_import_storylines(&storylines)?;
    let parent = output
        .parent()
        .context("import output must have a parent directory")?;
    let staging = tempfile::Builder::new()
        .prefix(".pchronicle-import-")
        .tempdir_in(parent)
        .with_context(|| format!("create import staging directory in {}", parent.display()))?;
    let staged_source = staging.path().join(source_path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged_source)
        .context("create staged import Source")?;
    file.write_all(&input)
        .context("write staged import Source")?;
    file.sync_all().context("sync staged import Source")?;
    validate_import_source(format, &staged_source).await?;
    std::fs::File::open(staging.path())
        .and_then(|directory| directory.sync_all())
        .context("sync import staging directory")?;

    let staging_path = staging.keep();
    let mut cleanup = PublishedPathGuard::new(staging_path.clone());
    rename_noreplace(&staging_path, &output)
        .with_context(|| format!("publish new Dataset {}", output.display()))?;
    cleanup.track(output.clone());
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync Dataset parent {}", parent.display()))?;
    cleanup.disarm();

    let document_format = exchange_document_format(format)
        .context("supported import format must map to a physical document format")?;
    let response = ImportResponse {
        dataset_uri: output.to_string_lossy().into_owned(),
        source_path: source_path.into(),
        format: document_format.as_str().into(),
        trajectories,
        input_bytes: input.len(),
    };
    serde_json::to_writer_pretty(&mut *stdout, &response)
        .context("encode pChronicle import JSON")?;
    writeln!(stdout).context("write pChronicle import JSON")?;
    writeln!(
        stderr,
        "dataset_uri={} source={} format={} trajectories={} input_bytes={}",
        response.dataset_uri,
        response.source_path,
        response.format,
        response.trajectories,
        response.input_bytes,
    )
    .context("write pChronicle import metadata")?;
    Ok(())
}

pub(super) async fn run_export(
    args: ExportArgs,
    settings_override: Option<&Path>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.format != ExchangeFormat::Auto,
        "export requires an explicit --format"
    );
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
        "--timeout-seconds must be greater than zero"
    );
    anyhow::ensure!(
        (args.output == "-") == args.stream,
        "--stream requires --output -, and --output - requires --stream"
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

    let format = export_format(args.format)?;
    let dataset = resolve_dataset_uri(args.from.as_deref(), settings_override)?;
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
    write_export_output(&args.output, &export.bytes, args.overwrite, stdout)?;
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
        "strict export requires an unfiltered Source already stored in the requested format"
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
        .map(|line| serde_json::from_slice(line).context("decode export Trajectory address"))
        .collect::<Result<Vec<ExportAddress>>>()?;
    ensure_export_trajectory_budget(addresses.len(), args.max_trajectories)?;
    anyhow::ensure!(
        !addresses.is_empty(),
        "export selection matched no Trajectories"
    );
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
                    "load export Trajectory {}/{}",
                    address.source_path, address.session_id
                )
            })?
            .with_context(|| {
                format!(
                    "export Trajectory disappeared from snapshot: {}/{}",
                    address.source_path, address.session_id
                )
            })?;
        anyhow::ensure!(
            story.trajectory_id.as_deref().unwrap_or(&story.session_id) == address.document_id,
            "export Trajectory document ID changed within the snapshot"
        );
        anyhow::ensure!(
            story.run_id == address.run_id,
            "export Trajectory Run ID changed within the snapshot"
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
        "export Source changed after the Catalog Snapshot was created"
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
        ExchangeFormat::Storyline => {
            if stories.len() == 1 {
                serde_json::to_value(&stories[0])?
            } else {
                serde_json::to_value(stories)?
            }
        }
        _ => unreachable!("exchange export format was validated"),
    };
    let mut output = serde_json::to_vec_pretty(&value).context("encode export JSON")?;
    output.push(b'\n');
    Ok(output)
}

fn export_format(format: ExchangeFormat) -> Result<ExchangeFormat> {
    Ok(match format {
        ExchangeFormat::Auto => bail!("export requires an explicit --format"),
        ExchangeFormat::Atif => ExchangeFormat::Atif,
        ExchangeFormat::Actf => ExchangeFormat::Actf,
        ExchangeFormat::OpenaiMessages => ExchangeFormat::OpenaiMessages,
        ExchangeFormat::Storyline => ExchangeFormat::Storyline,
    })
}

fn exchange_document_format(format: ExchangeFormat) -> Option<DocumentFormat> {
    match format {
        ExchangeFormat::Atif => Some(DocumentFormat::Atif),
        ExchangeFormat::Actf => Some(DocumentFormat::Actf),
        ExchangeFormat::OpenaiMessages => Some(DocumentFormat::OpenaiMsg),
        ExchangeFormat::Auto | ExchangeFormat::Storyline => None,
    }
}

fn write_export_output(
    output: &str,
    bytes: &[u8],
    overwrite: bool,
    stdout: &mut dyn Write,
) -> Result<()> {
    if output == "-" {
        stdout.write_all(bytes).context("write export stream")?;
        return Ok(());
    }
    anyhow::ensure!(
        !output.contains("://"),
        "export currently supports only local output files"
    );
    let output = Path::new(output);
    let filename = output
        .file_name()
        .context("export output must name a file")?;
    let parent = std::fs::canonicalize(output.parent().unwrap_or_else(|| Path::new(".")))
        .context("canonicalize export output parent directory")?;
    anyhow::ensure!(parent.is_dir(), "export output parent is not a directory");
    let output = parent.join(filename);
    if output.exists() {
        anyhow::ensure!(overwrite, "export output already exists; pass --overwrite");
        anyhow::ensure!(output.is_file(), "export output exists and is not a file");
    }
    let mut staging = tempfile::Builder::new()
        .prefix(".pchronicle-export-")
        .tempfile_in(&parent)
        .context("create export staging file")?;
    staging
        .write_all(bytes)
        .context("write export staging file")?;
    staging
        .as_file()
        .sync_all()
        .context("sync export staging file")?;
    let staging_path = staging.into_temp_path().keep()?;
    let mut cleanup = PublishedFileGuard::new(staging_path.clone());
    if overwrite {
        std::fs::rename(&staging_path, &output).context("replace export output atomically")?;
        // The old file is no longer available after a successful atomic replace,
        // so a later directory-sync error must not delete the newly published file.
        cleanup.disarm();
    } else {
        rename_noreplace(&staging_path, &output).context("publish new export output")?;
        cleanup.track(output);
    }
    std::fs::File::open(&parent)
        .and_then(|directory| directory.sync_all())
        .context("sync export output parent directory")?;
    cleanup.disarm();
    Ok(())
}

struct PublishedFileGuard {
    path: Option<PathBuf>,
}

impl PublishedFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn track(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for PublishedFileGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn local_file_snapshot_ref(path: &Path) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(path.to_string_lossy().as_bytes());
    if let Ok(metadata) = std::fs::metadata(path) {
        hash.update(&metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified() {
            if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
                hash.update(&duration.as_nanos().to_le_bytes());
            }
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

fn read_bounded(mut reader: impl Read, max_bytes: usize, label: &str) -> Result<Vec<u8>> {
    let limit = u64::try_from(max_bytes)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| {
            cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "--max-input-bytes is too large",
            )
        })?;
    let mut input = Vec::new();
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
) -> Result<ExchangeFormat> {
    let format = match requested {
        ExchangeFormat::Auto => match detect_format(input_path, Some(input))?.ok_or_else(|| {
            cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "cannot detect import format; pass --format explicitly",
            )
        })? {
            DocumentFormat::Atif => ExchangeFormat::Atif,
            DocumentFormat::Actf => ExchangeFormat::Actf,
            DocumentFormat::OpenaiMsg => ExchangeFormat::OpenaiMessages,
            format => {
                return Err(cli_boundary_error(
                    BoundaryCode::Unsupported,
                    format!("detected import format '{format}' is not a queryable JSON format"),
                ));
            }
        },
        ExchangeFormat::Atif => ExchangeFormat::Atif,
        ExchangeFormat::Actf => ExchangeFormat::Actf,
        ExchangeFormat::OpenaiMessages => ExchangeFormat::OpenaiMessages,
        ExchangeFormat::Storyline => ExchangeFormat::Storyline,
    };
    if !matches!(
        format,
        ExchangeFormat::Atif | ExchangeFormat::Actf | ExchangeFormat::OpenaiMessages
    ) {
        return Err(cli_boundary_error(
            BoundaryCode::Unsupported,
            format!(
                "import format '{format}' is not supported by the first queryable import increment"
            ),
        ));
    }
    Ok(format)
}

fn import_source_name(format: ExchangeFormat) -> &'static str {
    match format {
        ExchangeFormat::Atif => "trajectories.atif.json",
        ExchangeFormat::Actf => "trajectories.actf.json",
        ExchangeFormat::OpenaiMessages => "session_steps.json",
        _ => unreachable!("unsupported import format was rejected"),
    }
}

fn validate_import_storylines(storylines: &[StorylineDocument]) -> Result<usize> {
    let mut seen = HashSet::new();
    for storyline in storylines {
        if !seen.insert(storyline.document_id()) {
            return Err(cli_boundary_error(
                BoundaryCode::InvalidRequest,
                "import contains duplicate document_id",
            ));
        }
    }
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

fn validate_new_local_dataset_path(input: &str) -> Result<PathBuf> {
    let input = input.trim();
    anyhow::ensure!(!input.is_empty(), "import output path must not be empty");
    anyhow::ensure!(
        !input.contains("://"),
        "import currently supports only local output paths"
    );
    let path = Path::new(input);
    anyhow::ensure!(
        path.file_name().is_some(),
        "import output must name a new Dataset directory"
    );
    anyhow::ensure!(!path.exists(), "import output already exists");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = std::fs::canonicalize(parent)
        .with_context(|| "canonicalize import output parent directory")?;
    anyhow::ensure!(parent.is_dir(), "import output parent is not a directory");
    let filename = path
        .file_name()
        .context("import output must name a Dataset directory")?;
    Ok(parent.join(filename))
}

struct PublishedPathGuard {
    path: Option<PathBuf>,
}

impl PublishedPathGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }

    fn track(&mut self, path: PathBuf) {
        self.path = Some(path);
    }
}

impl Drop for PublishedPathGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = CString::new(from.as_os_str().as_bytes())?;
    let to = CString::new(to.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    // SAFETY: both pointers come from live CString values and are NUL-terminated.
    let result = unsafe {
        libc::renameat2(
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
