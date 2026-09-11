//! Dataset export command.

use super::super::*;
use super::decode::{exchange_document_format, validate_import_source};
use anyhow::{Context, Result, bail};
use persisting_pchronicle::document::{DocumentFormat, detect_format, encode_json_storylines};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::storage::{
    CatalogSourceKind, CatalogSourceStatus, CatalogStorylineKey, DEFAULT_DATASET_NAME,
    DatasetCatalogSnapshot,
};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub(crate) async fn run_export(
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

pub(crate) struct EncodedExport {
    bytes: Vec<u8>,
    trajectories: usize,
    exact: bool,
}

pub(crate) async fn export_from_snapshot(
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
    let engine = snapshot
        .clone()
        .query_engine(chronicle_query_options()?)
        .await?;
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

pub(crate) async fn exact_local_file_export(
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

pub(crate) fn ensure_export_trajectory_budget(
    trajectories: usize,
    max_trajectories: u64,
) -> Result<()> {
    if usize::try_from(max_trajectories).is_ok_and(|limit| trajectories > limit) {
        return Err(cli_boundary_error(
            BoundaryCode::ResourceExhausted,
            format!("export exceeds max_trajectories limit of {max_trajectories}"),
        ));
    }
    Ok(())
}

pub(crate) fn export_address_sql(args: &ExportArgs) -> Result<String> {
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

pub(crate) fn encode_export(
    format: ExchangeFormat,
    stories: &[StorylineDocument],
) -> Result<Vec<u8>> {
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

pub(crate) async fn write_export_output(
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

pub(crate) fn local_file_snapshot_ref(path: &Path) -> String {
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
