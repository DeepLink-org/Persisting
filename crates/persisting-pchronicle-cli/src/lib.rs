use std::fmt::Write as _;
use std::io::{Error as IoError, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use persisting_pchronicle::{
    CatalogErrorPolicy, CatalogSnapshotOptions, CatalogSourceKind, CatalogSourceStatus,
    ChronicleQueryEngine, DatasetCatalogSnapshot, DatasetMount, LocalQueryManifestOptions,
    DEFAULT_DATASET_NAME,
};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Parser)]
#[command(
    name = "pchronicle",
    version,
    about = "Browse, query, and exchange Agent trajectory Datasets"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List trajectory Sources discovered under a Dataset URI.
    #[command(visible_alias = "list")]
    Ls(ListArgs),
    /// Show Dataset health and aggregate statistics.
    Status(StatusArgs),
    /// Execute read-only SQL over one or more Datasets.
    Query(QueryArgs),
    /// Locate a Run, Trajectory, or Step by its Source-local ID.
    Find(FindArgs),
    /// Search trajectory text with lexical full-text search.
    Search(SearchArgs),
    /// Create a new Dataset from one trajectory exchange format.
    Import(ImportArgs),
    /// Export complete Trajectories to an exchange format.
    Export(ExportArgs),
    /// Compact and maintain an existing native Dataset.
    Maintain(DatasetArgs),
    /// Serve statically mounted Datasets through a read-only API and Web UI.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Local path or object-store URI of the Dataset.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,

    /// Include physical size, modification time, and version columns.
    #[arg(long)]
    physical: bool,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Continue when an individual Source cannot be frozen.
    #[arg(long, value_enum, default_value_t = ErrorMode::Report)]
    errors: ErrorMode,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
struct DatasetArgs {
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Local path or object-store URI of the Dataset.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Fail on a bad Source, or report partial counts and continue.
    #[arg(long, value_enum, default_value_t = ErrorMode::Report)]
    errors: ErrorMode,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,

    /// Maximum time for trajectory count queries.
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct QueryArgs {
    /// Dataset URI, or SQL when using --dataset mounts.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Mount a named Dataset as NAME=URI. Repeat for cross-Dataset SQL.
    #[arg(long = "dataset", value_name = "NAME=URI")]
    datasets: Vec<String>,

    /// One read-only SQL statement.
    #[arg(value_name = "SQL")]
    sql: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSONL when piped.
    #[arg(long, value_enum, default_value_t = QueryOutputFormat::Auto)]
    format: QueryOutputFormat,

    /// Write results to a new file instead of stdout. Use - for stdout.
    #[arg(short, long, value_name = "PATH_OR_STDOUT", default_value = "-")]
    output: String,

    /// Reject results containing more rows than this limit.
    #[arg(long, default_value_t = 100_000)]
    max_output_rows: u64,

    /// Reject intermediate or final encoded results larger than this many bytes.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_output_bytes: usize,

    /// Maximum time for SQL execution and result encoding.
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("identity")
        .required(true)
        .multiple(false)
        .args(["run_id", "session_id"])
))]
struct FindArgs {
    /// Local path or object-store URI of the Dataset.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,

    /// Narrow the lookup to one Dataset-relative Source path.
    #[arg(long)]
    source: Option<String>,

    /// Find Run or Trajectory candidates by Source-local Run ID.
    #[arg(long)]
    run_id: Option<String>,

    /// Find Trajectory or Step candidates by Source-local Session ID.
    #[arg(long)]
    session_id: Option<String>,

    /// Find one Step within the selected Session.
    #[arg(long, requires = "session_id")]
    step_id: Option<i64>,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Maximum number of matches returned before marking the result truncated.
    #[arg(long, default_value_t = 100)]
    max_results: usize,

    /// Reject intermediate or final encoded results larger than this many bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    max_output_bytes: usize,

    /// Maximum time for the lookup query.
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
struct SearchArgs {
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,
    #[arg(value_name = "QUERY")]
    query: String,
    #[arg(long, default_value_t = 20)]
    top_k: usize,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExchangeFormat {
    Auto,
    Atif,
    Actf,
    #[value(name = "openai-messages")]
    OpenaiMessages,
    Storyline,
}

#[derive(Debug, Args)]
struct ImportArgs {
    #[arg(short = 'f', long = "from", value_name = "PATH_OR_STDIN")]
    from: String,
    #[arg(short, long, value_name = "NEW_DATASET_URI")]
    output: String,
    #[arg(long, value_enum)]
    format: ExchangeFormat,
    #[arg(long)]
    stream: bool,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[arg(short = 'f', long = "from", value_name = "DATASET_URI")]
    from: String,
    #[arg(short, long, value_name = "PATH_OR_STDOUT")]
    output: String,
    #[arg(long, value_enum)]
    format: ExchangeFormat,
    #[arg(long)]
    source: Option<String>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long, value_name = "EXPRESSION")]
    r#where: Option<String>,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    overwrite: bool,
    #[arg(long)]
    stream: bool,
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
    #[arg(long)]
    open: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    #[default]
    Auto,
    Table,
    Json,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum QueryOutputFormat {
    #[default]
    Auto,
    Table,
    Jsonl,
    Csv,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum ErrorMode {
    Strict,
    #[default]
    Report,
}

impl From<ErrorMode> for CatalogErrorPolicy {
    fn from(value: ErrorMode) -> Self {
        match value {
            ErrorMode::Strict => Self::Strict,
            ErrorMode::Report => Self::Report,
        }
    }
}

#[derive(Debug, Serialize)]
struct ListResponse {
    schema_version: &'static str,
    dataset_uri: String,
    snapshot_id: String,
    created_at: String,
    sources: Vec<SourceResponse>,
}

#[derive(Debug, Serialize)]
struct SourceResponse {
    source_path: String,
    format: Option<String>,
    kind: CatalogSourceKind,
    snapshot_ref: Option<String>,
    size_bytes: Option<u64>,
    last_modified: Option<String>,
    status: CatalogSourceStatus,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
struct StatusCounts {
    runs: u64,
    trajectories: u64,
    steps: u64,
    tool_calls: u64,
    events: u64,
}

impl std::ops::AddAssign for StatusCounts {
    fn add_assign(&mut self, other: Self) {
        self.runs = self.runs.saturating_add(other.runs);
        self.trajectories = self.trajectories.saturating_add(other.trajectories);
        self.steps = self.steps.saturating_add(other.steps);
        self.tool_calls = self.tool_calls.saturating_add(other.tool_calls);
        self.events = self.events.saturating_add(other.events);
    }
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    schema_version: &'static str,
    dataset_uri: String,
    snapshot_id: String,
    created_at: String,
    status: &'static str,
    counts_complete: bool,
    sources: StatusSources,
    counts: StatusCounts,
    source_errors: Vec<StatusSourceError>,
}

#[derive(Debug, Serialize)]
struct StatusSources {
    total: usize,
    ready: usize,
    error: usize,
}

#[derive(Debug, Serialize)]
struct StatusSourceError {
    source_path: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct FindResponse {
    schema_version: &'static str,
    dataset_uri: String,
    snapshot_id: String,
    query: FindQueryResponse,
    truncated: bool,
    matches: Vec<FindMatch>,
}

#[derive(Debug, Serialize)]
struct FindQueryResponse {
    source: Option<String>,
    run_id: Option<String>,
    session_id: Option<String>,
    step_id: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct FindMatch {
    source_path: String,
    run_id: String,
    session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    effective_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

pub async fn run(
    cli: Cli,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    match cli.command {
        Command::Ls(args) => run_list(args, stdout_is_terminal, stdout, stderr).await,
        Command::Status(args) => run_status(args, stdout_is_terminal, stdout, stderr).await,
        Command::Query(args) => run_query(args, stdout_is_terminal, stdout, stderr).await,
        Command::Find(args) => run_find(args, stdout_is_terminal, stdout, stderr).await,
        Command::Search(args) => {
            let _ = (args.query, args.top_k);
            not_implemented("search", Some(&args.dataset_uri))
        }
        Command::Import(args) => {
            let _ = (args.from, args.output, args.format, args.stream);
            not_implemented("import", None)
        }
        Command::Export(args) => {
            let _ = (
                args.from,
                args.output,
                args.format,
                args.source,
                args.run_id,
                args.session_id,
                args.r#where,
                args.strict,
                args.overwrite,
                args.stream,
            );
            not_implemented("export", None)
        }
        Command::Maintain(args) => not_implemented("maintain", Some(&args.dataset_uri)),
        Command::Serve(args) => {
            let _ = (args.config, args.open);
            not_implemented("serve", None)
        }
    }
}

fn not_implemented(command: &str, _dataset_uri: Option<&str>) -> Result<()> {
    bail!("pchronicle {command} is not implemented yet")
}

async fn run_list(
    args: ListArgs,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let (dataset_uri, snapshot) = discover_snapshot(
        &args.dataset_uri,
        args.errors,
        args.max_files,
        args.max_entries,
    )
    .await?;
    let dataset = snapshot
        .dataset(DEFAULT_DATASET_NAME)
        .context("default Dataset missing from Catalog Snapshot")?;
    let response = ListResponse {
        schema_version: "pchronicle.ls.v1",
        dataset_uri,
        snapshot_id: snapshot.snapshot_id().to_string(),
        created_at: snapshot.created_at().to_string(),
        sources: dataset
            .sources
            .iter()
            .map(|source| SourceResponse {
                source_path: source.file.clone(),
                format: source.format.clone(),
                kind: source.kind,
                snapshot_ref: source.snapshot_ref.clone(),
                size_bytes: source.size_bytes,
                last_modified: source.last_modified.clone(),
                status: source.status,
                error: source.error.clone(),
            })
            .collect(),
    };

    let output_format = match args.format {
        OutputFormat::Auto if stdout_is_terminal => OutputFormat::Table,
        OutputFormat::Auto => OutputFormat::Json,
        explicit => explicit,
    };
    match output_format {
        OutputFormat::Table => write_table(stdout, &response.sources, args.physical)?,
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *stdout, &response)
                .context("encode pChronicle ls JSON")?;
            writeln!(stdout).context("write pChronicle ls JSON")?;
        }
        OutputFormat::Auto => unreachable!("auto output format was resolved"),
    }
    writeln!(
        stderr,
        "snapshot_id={} dataset_uri={} sources={} ready={} errors={}",
        response.snapshot_id,
        response.dataset_uri,
        response.sources.len(),
        dataset.ready_source_count(),
        dataset.error_source_count(),
    )
    .context("write pChronicle ls metadata")?;
    Ok(())
}

async fn run_status(
    args: StatusArgs,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.timeout_seconds > 0,
        "--timeout-seconds must be greater than zero"
    );
    let (dataset_uri, snapshot) = discover_snapshot(
        &args.dataset_uri,
        args.errors,
        args.max_files,
        args.max_entries,
    )
    .await?;
    let snapshot = Arc::new(snapshot);
    let dataset = snapshot
        .dataset(DEFAULT_DATASET_NAME)
        .context("default Dataset missing from Catalog Snapshot")?;
    let total_sources = dataset.sources.len();
    let mut source_errors = dataset
        .sources
        .iter()
        .filter(|source| source.status == CatalogSourceStatus::Error)
        .map(|source| StatusSourceError {
            source_path: source.file.clone(),
            error: source
                .error
                .as_deref()
                .map(|error| redact_message(error, &dataset_uri))
                .unwrap_or_else(|| "Source discovery failed".into()),
        })
        .collect::<Vec<_>>();
    let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
    let timeout = Duration::from_secs(args.timeout_seconds);
    let deadline = tokio::time::Instant::now() + timeout;
    let counts = match query_status_counts(&engine, None, deadline, timeout).await {
        Ok(counts) if source_errors.is_empty() => counts,
        Ok(_) | Err(_) if args.errors == ErrorMode::Report => {
            let mut counts = StatusCounts::default();
            for source in dataset
                .sources
                .iter()
                .filter(|source| source.status == CatalogSourceStatus::Ready)
            {
                if source_errors
                    .iter()
                    .any(|error| error.source_path == source.file)
                {
                    continue;
                }
                match query_status_counts(&engine, Some(&source.file), deadline, timeout).await {
                    Ok(source_counts) => counts += source_counts,
                    Err(error) => source_errors.push(StatusSourceError {
                        source_path: source.file.clone(),
                        error: redact_message(&format!("{error:#}"), &dataset_uri),
                    }),
                }
            }
            counts
        }
        Err(error) => return Err(error),
        Ok(counts) => counts,
    };
    source_errors.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    source_errors.dedup_by(|left, right| left.source_path == right.source_path);
    let error_sources = source_errors.len();
    let ready_sources = total_sources.saturating_sub(error_sources);
    let response = StatusResponse {
        schema_version: "pchronicle.status.v1",
        dataset_uri,
        snapshot_id: snapshot.snapshot_id().to_string(),
        created_at: snapshot.created_at().to_string(),
        status: match (ready_sources, error_sources) {
            (_, 0) => "ready",
            (0, _) => "error",
            _ => "degraded",
        },
        counts_complete: error_sources == 0,
        sources: StatusSources {
            total: total_sources,
            ready: ready_sources,
            error: error_sources,
        },
        counts,
        source_errors,
    };

    let output_format = match args.format {
        OutputFormat::Auto if stdout_is_terminal => OutputFormat::Table,
        OutputFormat::Auto => OutputFormat::Json,
        explicit => explicit,
    };
    match output_format {
        OutputFormat::Table => write_status_table(stdout, &response)?,
        OutputFormat::Json => {
            serde_json::to_writer_pretty(&mut *stdout, &response)
                .context("encode pChronicle status JSON")?;
            writeln!(stdout).context("write pChronicle status JSON")?;
        }
        OutputFormat::Auto => unreachable!("auto output format was resolved"),
    }
    writeln!(
        stderr,
        "snapshot_id={} dataset_uri={} status={} counts_complete={}",
        response.snapshot_id, response.dataset_uri, response.status, response.counts_complete,
    )
    .context("write pChronicle status metadata")?;
    Ok(())
}

async fn run_query(
    args: QueryArgs,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let (dataset_uri, sql) = query_inputs(&args)?;
    anyhow::ensure!(
        args.max_output_rows > 0,
        "--max-output-rows must be greater than zero"
    );
    anyhow::ensure!(
        args.max_output_bytes > 0,
        "--max-output-bytes must be greater than zero"
    );
    anyhow::ensure!(
        args.timeout_seconds > 0,
        "--timeout-seconds must be greater than zero"
    );
    let (dataset_label, dataset_uris, snapshot) = discover_query_snapshot(
        dataset_uri,
        &args.datasets,
        args.max_files,
        args.max_entries,
    )
    .await?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot)
        .await
        .map_err(|error| redact_query_error(&error, &dataset_uris, None))?;
    let mut buffer = LimitedBuffer::new(args.max_output_bytes);
    let query_result = tokio::time::timeout(
        Duration::from_secs(args.timeout_seconds),
        engine.write_query_jsonl_with_max_rows(sql, &mut buffer, Some(args.max_output_rows)),
    )
    .await;
    match query_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(redact_query_error(&error, &dataset_uris, Some(sql)));
        }
        Err(_) => bail!(
            "Dataset query timed out after {} seconds",
            args.timeout_seconds
        ),
    }
    let jsonl = String::from_utf8(buffer.into_inner()).context("query JSONL is not UTF-8")?;
    let rows = parse_jsonl_rows(&jsonl)?;
    let format = match args.format {
        QueryOutputFormat::Auto if stdout_is_terminal && args.output == "-" => {
            QueryOutputFormat::Table
        }
        QueryOutputFormat::Auto => QueryOutputFormat::Jsonl,
        explicit => explicit,
    };
    let output = match format {
        QueryOutputFormat::Table => encode_query_table(&rows)?,
        QueryOutputFormat::Jsonl => jsonl.into_bytes(),
        QueryOutputFormat::Csv => encode_query_csv(&rows),
        QueryOutputFormat::Auto => unreachable!("auto output format was resolved"),
    };
    anyhow::ensure!(
        output.len() <= args.max_output_bytes,
        "encoded SQL result exceeds max_output_bytes limit of {}",
        args.max_output_bytes
    );
    write_query_output(&args.output, &output, stdout)?;
    writeln!(
        stderr,
        "snapshot_id={} datasets={} rows={} format={} output_bytes={}",
        snapshot_id,
        dataset_label,
        rows.values.len(),
        query_format_name(format),
        output.len(),
    )
    .context("write pChronicle query metadata")?;
    Ok(())
}

async fn run_find(
    args: FindArgs,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.max_results > 0,
        "--max-results must be greater than zero"
    );
    anyhow::ensure!(
        args.max_output_bytes > 0,
        "--max-output-bytes must be greater than zero"
    );
    anyhow::ensure!(
        args.timeout_seconds > 0,
        "--timeout-seconds must be greater than zero"
    );
    if let Some(source) = &args.source {
        validate_source_path(source)?;
    }
    if let Some(run_id) = &args.run_id {
        validate_find_id("--run-id", run_id)?;
    }
    if let Some(session_id) = &args.session_id {
        validate_find_id("--session-id", session_id)?;
    }
    let (_, dataset_uris, snapshot) = discover_query_snapshot(
        Some(&args.dataset_uri),
        &[],
        args.max_files,
        args.max_entries,
    )
    .await?;
    let dataset_uri = dataset_uris
        .first()
        .cloned()
        .context("find Dataset URI missing after discovery")?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot)
        .await
        .map_err(|error| redact_query_error(&error, std::slice::from_ref(&dataset_uri), None))?;
    let sql = find_sql(&args)?;
    let mut buffer = LimitedBuffer::new(args.max_output_bytes);
    let max_query_rows = args
        .max_results
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .context("--max-results is too large")?;
    let query_result = tokio::time::timeout(
        Duration::from_secs(args.timeout_seconds),
        engine.write_query_jsonl_with_max_rows(&sql, &mut buffer, Some(max_query_rows)),
    )
    .await;
    let jsonl = match query_result {
        Ok(Ok(())) => String::from_utf8(buffer.into_inner()).context("find JSONL is not UTF-8")?,
        Ok(Err(error)) => {
            return Err(redact_query_error(
                &error,
                std::slice::from_ref(&dataset_uri),
                Some(&sql),
            ));
        }
        Err(_) => bail!(
            "Dataset find timed out after {} seconds",
            args.timeout_seconds
        ),
    };
    let mut matches = jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("decode pChronicle find match"))
        .collect::<Result<Vec<FindMatch>>>()?;
    let truncated = matches.len() > args.max_results;
    matches.truncate(args.max_results);
    let response = FindResponse {
        schema_version: "pchronicle.find.v1",
        dataset_uri,
        snapshot_id,
        query: FindQueryResponse {
            source: args.source,
            run_id: args.run_id,
            session_id: args.session_id,
            step_id: args.step_id,
        },
        truncated,
        matches,
    };

    let output_format = match args.format {
        OutputFormat::Auto if stdout_is_terminal => OutputFormat::Table,
        OutputFormat::Auto => OutputFormat::Json,
        explicit => explicit,
    };
    match output_format {
        OutputFormat::Table => {
            let mut output = Vec::new();
            write_find_table(&mut output, &response)?;
            anyhow::ensure!(
                output.len() <= args.max_output_bytes,
                "encoded find result exceeds max_output_bytes limit of {}",
                args.max_output_bytes
            );
            stdout
                .write_all(&output)
                .context("write pChronicle find table")?;
        }
        OutputFormat::Json => {
            let mut output =
                serde_json::to_vec_pretty(&response).context("encode pChronicle find JSON")?;
            output.push(b'\n');
            anyhow::ensure!(
                output.len() <= args.max_output_bytes,
                "encoded find result exceeds max_output_bytes limit of {}",
                args.max_output_bytes
            );
            stdout
                .write_all(&output)
                .context("write pChronicle find JSON")?;
        }
        OutputFormat::Auto => unreachable!("auto output format was resolved"),
    }
    writeln!(
        stderr,
        "snapshot_id={} matches={} truncated={}",
        response.snapshot_id,
        response.matches.len(),
        response.truncated,
    )
    .context("write pChronicle find metadata")?;
    Ok(())
}

fn find_sql(args: &FindArgs) -> Result<String> {
    let mut predicates = Vec::new();
    if let Some(source) = &args.source {
        predicates.push(format!("_file_ = {}", sql_string(source)));
    }
    if let Some(run_id) = &args.run_id {
        predicates.push(format!("run_id = {}", sql_string(run_id)));
    }
    if let Some(session_id) = &args.session_id {
        predicates.push(format!("session_id = {}", sql_string(session_id)));
    }
    if let Some(step_id) = args.step_id {
        predicates.push(format!("step_id = {step_id}"));
    }
    anyhow::ensure!(
        !predicates.is_empty(),
        "find requires an identity predicate"
    );
    let table = if args.step_id.is_some() {
        "steps"
    } else {
        "runs"
    };
    let projection = if args.step_id.is_some() {
        "_file_ AS source_path, run_id, session_id, step_id, \
         source AS step_source, effective_kind, timestamp"
    } else {
        "_file_ AS source_path, run_id, session_id, \
         CAST(NULL AS BIGINT) AS step_id, \
         CAST(NULL AS VARCHAR) AS step_source, \
         CAST(NULL AS VARCHAR) AS effective_kind, \
         CAST(NULL AS VARCHAR) AS timestamp"
    };
    let limit = args
        .max_results
        .checked_add(1)
        .context("--max-results is too large")?;
    Ok(format!(
        "SELECT {projection} FROM dataset.{table} WHERE {} \
         ORDER BY _file_, session_id{} LIMIT {limit}",
        predicates.join(" AND "),
        if args.step_id.is_some() {
            ", step_id"
        } else {
            ""
        }
    ))
}

fn validate_source_path(source: &str) -> Result<()> {
    anyhow::ensure!(!source.trim().is_empty(), "--source must not be empty");
    anyhow::ensure!(
        source == source.trim(),
        "--source must not contain surrounding whitespace"
    );
    anyhow::ensure!(source.len() <= 4096, "--source must not exceed 4096 bytes");
    anyhow::ensure!(
        !source.contains('\0'),
        "--source must not contain NUL bytes"
    );
    anyhow::ensure!(
        !source.starts_with('/') && !source.contains("://"),
        "--source must be a Dataset-relative Source path"
    );
    anyhow::ensure!(
        !source.split('/').any(|component| component == ".."),
        "--source must not traverse outside the Dataset"
    );
    Ok(())
}

fn validate_find_id(flag: &str, value: &str) -> Result<()> {
    anyhow::ensure!(!value.is_empty(), "{flag} must not be empty");
    anyhow::ensure!(value.len() <= 4096, "{flag} must not exceed 4096 bytes");
    anyhow::ensure!(!value.contains('\0'), "{flag} must not contain NUL bytes");
    Ok(())
}

fn write_find_table(stdout: &mut dyn Write, response: &FindResponse) -> Result<()> {
    let step_lookup = response.query.step_id.is_some();
    let mut rows = Vec::with_capacity(response.matches.len() + 1);
    if step_lookup {
        rows.push(
            [
                "SOURCE",
                "RUN ID",
                "SESSION ID",
                "STEP ID",
                "STEP SOURCE",
                "KIND",
                "TIMESTAMP",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        );
    } else {
        rows.push(
            ["SOURCE", "RUN ID", "SESSION ID"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
    }
    for candidate in &response.matches {
        let mut row = vec![
            truncate(&candidate.source_path, 64),
            candidate.run_id.clone(),
            candidate.session_id.clone(),
        ];
        if step_lookup {
            row.extend([
                candidate
                    .step_id
                    .map(|step_id| step_id.to_string())
                    .unwrap_or_else(|| "-".into()),
                candidate.step_source.as_deref().unwrap_or("-").to_string(),
                candidate
                    .effective_kind
                    .as_deref()
                    .unwrap_or("-")
                    .to_string(),
                candidate.timestamp.as_deref().unwrap_or("-").to_string(),
            ]);
        }
        rows.push(row);
    }
    write_grid(stdout, &rows, "write pChronicle find table")?;
    if response.matches.is_empty() {
        writeln!(stdout, "(0 matches)").context("write empty pChronicle find table")?;
    } else if response.truncated {
        writeln!(stdout, "(truncated)").context("write truncated pChronicle find table")?;
    }
    Ok(())
}

fn redact_query_error(
    error: &anyhow::Error,
    dataset_uris: &[String],
    sql: Option<&str>,
) -> anyhow::Error {
    let mut message = format!("{error:#}");
    if let Some(sql) = sql {
        message = message.replace(sql, "<sql>");
    }
    for dataset_uri in dataset_uris {
        message = redact_message(&message, dataset_uri);
    }
    anyhow!(message)
}

fn query_inputs(args: &QueryArgs) -> Result<(Option<&str>, &str)> {
    if args.datasets.is_empty() {
        let dataset_uri = args
            .dataset_uri
            .as_deref()
            .context("query requires <DATASET_URI> <SQL> or --dataset NAME=URI <SQL>")?;
        let sql = args
            .sql
            .as_deref()
            .context("query requires SQL after the Dataset URI")?;
        Ok((Some(dataset_uri), sql))
    } else {
        anyhow::ensure!(
            args.sql.is_none(),
            "named Dataset query accepts one SQL positional argument"
        );
        let sql = args
            .dataset_uri
            .as_deref()
            .context("named Dataset query requires SQL")?;
        Ok((None, sql))
    }
}

async fn discover_query_snapshot(
    dataset_uri: Option<&str>,
    datasets: &[String],
    max_files: usize,
    max_entries: usize,
) -> Result<(String, Vec<String>, DatasetCatalogSnapshot)> {
    let (mounts, default_dataset) = if let Some(dataset_uri) = dataset_uri {
        let dataset_uri = normalize_and_validate_dataset_uri(dataset_uri)?;
        (
            vec![DatasetMount::default(dataset_uri)?],
            Some(DEFAULT_DATASET_NAME.into()),
        )
    } else {
        let mut mounts = Vec::with_capacity(datasets.len());
        for dataset in datasets {
            let (name, uri) = dataset
                .split_once('=')
                .context("--dataset must use NAME=URI")?;
            anyhow::ensure!(!name.is_empty(), "--dataset name must not be empty");
            let uri = normalize_and_validate_dataset_uri(uri)?;
            mounts.push(DatasetMount::new(name, uri)?);
        }
        (mounts, None)
    };
    let dataset_uris = mounts
        .iter()
        .map(|mount| mount.uri.clone())
        .collect::<Vec<_>>();
    let dataset_label = mounts
        .iter()
        .map(|mount| mount.name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let snapshot = DatasetCatalogSnapshot::discover(
        mounts,
        default_dataset,
        CatalogSnapshotOptions {
            error_policy: CatalogErrorPolicy::Strict,
            manifest: LocalQueryManifestOptions {
                max_files,
                max_entries,
                ..LocalQueryManifestOptions::default()
            },
            ..CatalogSnapshotOptions::default()
        },
    )
    .await
    .map_err(|error| redact_query_error(&error, &dataset_uris, None))
    .context("discover query Dataset Sources")?;
    Ok((dataset_label, dataset_uris, snapshot))
}

struct LimitedBuffer {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl LimitedBuffer {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl Write for LimitedBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next_size = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| IoError::other("query output size overflow"))?;
        if next_size > self.max_bytes {
            return Err(IoError::other(format!(
                "SQL result exceeds max_output_bytes limit of {}",
                self.max_bytes
            )));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct QueryRows {
    columns: Vec<String>,
    values: Vec<serde_json::Map<String, serde_json::Value>>,
}

fn parse_jsonl_rows(jsonl: &str) -> Result<QueryRows> {
    use serde::de::{MapAccess, Visitor};
    use serde::Deserializer as _;

    struct OrderedObjectVisitor;

    impl<'de> Visitor<'de> for OrderedObjectVisitor {
        type Value = Vec<(String, serde_json::Value)>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a JSON object")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(entry) = map.next_entry()? {
                values.push(entry);
            }
            Ok(values)
        }
    }

    let mut columns = Vec::new();
    let mut values = Vec::new();
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let mut deserializer = serde_json::Deserializer::from_str(line);
        let entries = deserializer
            .deserialize_map(OrderedObjectVisitor)
            .context("decode pChronicle query row")?;
        deserializer.end().context("decode pChronicle query row")?;
        let mut row = serde_json::Map::new();
        for (column, value) in entries {
            if !columns.contains(&column) {
                columns.push(column.clone());
            }
            row.insert(column, value);
        }
        values.push(row);
    }
    Ok(QueryRows { columns, values })
}

fn encode_query_csv(rows: &QueryRows) -> Vec<u8> {
    if rows.columns.is_empty() {
        return Vec::new();
    }
    let mut output = String::new();
    write_csv_row(&mut output, rows.columns.iter().cloned());
    for row in &rows.values {
        write_csv_row(
            &mut output,
            rows.columns
                .iter()
                .map(|column| query_value(row.get(column))),
        );
    }
    output.into_bytes()
}

fn write_csv_row(output: &mut String, values: impl IntoIterator<Item = String>) {
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        if value.contains([',', '"', '\n', '\r']) {
            output.push('"');
            output.push_str(&value.replace('"', "\"\""));
            output.push('"');
        } else {
            output.push_str(&value);
        }
    }
    output.push('\n');
}

fn encode_query_table(rows: &QueryRows) -> Result<Vec<u8>> {
    if rows.columns.is_empty() {
        return Ok(b"(0 rows)\n".to_vec());
    }
    let mut grid = Vec::with_capacity(rows.values.len() + 1);
    grid.push(rows.columns.clone());
    grid.extend(rows.values.iter().map(|row| {
        rows.columns
            .iter()
            .map(|column| truncate(&query_value(row.get(column)), 80))
            .collect()
    }));
    let mut output = Vec::new();
    write_grid(&mut output, &grid, "write pChronicle query table")?;
    Ok(output)
}

fn query_value(value: Option<&serde_json::Value>) -> String {
    match value {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
    }
}

fn write_query_output(path: &str, output: &[u8], stdout: &mut dyn Write) -> Result<()> {
    if path == "-" {
        stdout.write_all(output).context("write query output")?;
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create query output file {path}"))?;
    file.write_all(output)
        .with_context(|| format!("write query output file {path}"))?;
    file.flush()
        .with_context(|| format!("flush query output file {path}"))
}

fn query_format_name(format: QueryOutputFormat) -> &'static str {
    match format {
        QueryOutputFormat::Table => "table",
        QueryOutputFormat::Jsonl => "jsonl",
        QueryOutputFormat::Csv => "csv",
        QueryOutputFormat::Auto => "auto",
    }
}

async fn discover_snapshot(
    input: &str,
    errors: ErrorMode,
    max_files: usize,
    max_entries: usize,
) -> Result<(String, DatasetCatalogSnapshot)> {
    let dataset_uri = normalize_and_validate_dataset_uri(input)?;
    let mount = DatasetMount::default(dataset_uri.clone())?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![mount],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions {
            error_policy: errors.into(),
            manifest: LocalQueryManifestOptions {
                max_files,
                max_entries,
                ..LocalQueryManifestOptions::default()
            },
            ..CatalogSnapshotOptions::default()
        },
    )
    .await
    .with_context(|| "discover Dataset Sources")?;
    Ok((dataset_uri, snapshot))
}

async fn query_status_counts(
    engine: &ChronicleQueryEngine,
    source_path: Option<&str>,
    deadline: tokio::time::Instant,
    timeout: Duration,
) -> Result<StatusCounts> {
    let predicate = source_path
        .map(|source| format!(" WHERE _file_ = {}", sql_string(source)))
        .unwrap_or_default();
    let sql = format!(
        "SELECT \
           (SELECT COUNT(*) FROM dataset.runs{predicate}) AS runs, \
           (SELECT COUNT(*) FROM dataset.steps{predicate}) AS steps, \
           (SELECT COUNT(*) FROM dataset.tool_calls{predicate}) AS tool_calls, \
           (SELECT COUNT(*) FROM dataset.events{predicate}) AS events"
    );
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    anyhow::ensure!(
        !remaining.is_zero(),
        "Dataset status query timed out after {} seconds",
        timeout.as_secs()
    );
    let output = tokio::time::timeout(remaining, engine.query_jsonl(&sql))
        .await
        .with_context(|| {
            format!(
                "Dataset status query timed out after {} seconds",
                timeout.as_secs()
            )
        })??;
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let line = lines
        .next()
        .context("Dataset status query returned no row")?;
    anyhow::ensure!(
        lines.next().is_none(),
        "Dataset status query returned multiple rows"
    );
    #[derive(Deserialize)]
    struct QueryCounts {
        runs: u64,
        steps: u64,
        tool_calls: u64,
        events: u64,
    }
    let counts: QueryCounts = serde_json::from_str(line).context("decode Dataset status counts")?;
    Ok(StatusCounts {
        runs: counts.runs,
        trajectories: counts.runs,
        steps: counts.steps,
        tool_calls: counts.tool_calls,
        events: counts.events,
    })
}

fn write_status_table(stdout: &mut dyn Write, response: &StatusResponse) -> Result<()> {
    writeln!(stdout, "FIELD         VALUE       ACCURACY")?;
    writeln!(stdout, "status        {}", response.status)?;
    writeln!(
        stdout,
        "sources       {}          exact",
        response.sources.total
    )?;
    writeln!(
        stdout,
        "ready_sources {}          exact",
        response.sources.ready
    )?;
    writeln!(
        stdout,
        "error_sources {}          exact",
        response.sources.error
    )?;
    let accuracy = if response.counts_complete {
        "exact"
    } else {
        "partial"
    };
    writeln!(
        stdout,
        "runs          {}          {accuracy}",
        response.counts.runs
    )?;
    writeln!(
        stdout,
        "trajectories  {}          {accuracy}",
        response.counts.trajectories
    )?;
    writeln!(
        stdout,
        "steps         {}          {accuracy}",
        response.counts.steps
    )?;
    writeln!(
        stdout,
        "tool_calls    {}          {accuracy}",
        response.counts.tool_calls
    )?;
    writeln!(
        stdout,
        "events        {}          {accuracy}",
        response.counts.events
    )?;
    for error in &response.source_errors {
        writeln!(
            stdout,
            "source_error  {}: {}",
            truncate(&error.source_path, 48),
            truncate(&error.error, 80)
        )?;
    }
    Ok(())
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn redact_message(message: &str, dataset_uri: &str) -> String {
    message
        .replace(dataset_uri, "<dataset>")
        .split_whitespace()
        .map(|part| part.split('?').next().unwrap_or(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_and_validate_dataset_uri(input: &str) -> Result<String> {
    let input = input.trim();
    anyhow::ensure!(!input.is_empty(), "Dataset URI must not be empty");
    if !input.contains("://") {
        return Ok(std::fs::canonicalize(input)
            .with_context(|| "canonicalize local Dataset path")?
            .to_string_lossy()
            .into_owned());
    }

    let url = Url::parse(input).context("parse Dataset URI")?;
    anyhow::ensure!(
        matches!(url.scheme(), "local" | "file" | "s3" | "az" | "gs"),
        "unsupported Dataset URI scheme '{}'",
        url.scheme()
    );
    anyhow::ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Dataset URI must not contain embedded credentials"
    );
    anyhow::ensure!(
        url.query().is_none(),
        "Dataset URI must not contain a query string or signed credentials"
    );
    anyhow::ensure!(
        url.fragment().is_none(),
        "Dataset URI must not contain a fragment"
    );
    if matches!(url.scheme(), "s3" | "az" | "gs") {
        anyhow::ensure!(
            url.host_str().is_some(),
            "object-store URI must name a bucket"
        );
    } else {
        anyhow::ensure!(
            url.host_str().is_none(),
            "local Dataset URI must not contain a host"
        );
    }
    let minimum_length = input.find("://").map_or(1, |index| {
        index
            + if matches!(url.scheme(), "local" | "file") {
                4
            } else {
                3
            }
    });
    let mut normalized = input.to_string();
    while normalized.len() > minimum_length && normalized.ends_with('/') {
        normalized.pop();
    }
    Ok(normalized)
}

fn write_table(stdout: &mut dyn Write, sources: &[SourceResponse], physical: bool) -> Result<()> {
    let mut rows = Vec::with_capacity(sources.len() + 1);
    let mut header = vec!["SOURCE", "FORMAT", "KIND", "STATUS"];
    if physical {
        header.extend(["SIZE", "LAST MODIFIED", "SNAPSHOT"]);
    }
    header.push("ERROR");
    rows.push(header.into_iter().map(str::to_string).collect::<Vec<_>>());

    for source in sources {
        let mut row = vec![
            truncate(&source.source_path, 64),
            source.format.as_deref().unwrap_or("-").to_string(),
            enum_json(source.kind),
            enum_json(source.status),
        ];
        if physical {
            row.extend([
                source
                    .size_bytes
                    .map(format_bytes)
                    .unwrap_or_else(|| "-".into()),
                source.last_modified.as_deref().unwrap_or("-").to_string(),
                truncate(source.snapshot_ref.as_deref().unwrap_or("-"), 40),
            ]);
        }
        row.push(truncate(source.error.as_deref().unwrap_or("-"), 80));
        rows.push(row);
    }

    write_grid(stdout, &rows, "write pChronicle ls table")
}

fn write_grid(stdout: &mut dyn Write, rows: &[Vec<String>], context: &'static str) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let widths = (0..rows[0].len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();
    for row in rows {
        let mut line = String::new();
        for (column, cell) in row.iter().enumerate() {
            if column > 0 {
                line.push_str("  ");
            }
            let padding = widths[column].saturating_sub(cell.chars().count());
            write!(line, "{cell}{}", " ".repeat(padding))?;
        }
        writeln!(stdout, "{}", line.trim_end()).context(context)?;
    }
    Ok(())
}

fn enum_json<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".into())
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use serde_json::Value;
    use std::fs;

    fn atif_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../persisting-pchronicle/tests/fixtures/atif/dialogue_10.json")
    }

    fn example_dataset(format: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/data")
            .join(format)
    }

    #[test]
    fn command_tree_contains_the_product_commands() {
        let command = Cli::command();
        let names = command
            .get_subcommands()
            .map(|command| command.get_name())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["ls", "status", "query", "find", "search", "import", "export", "maintain", "serve"]
        );
        let ls = command
            .get_subcommands()
            .find(|command| command.get_name() == "ls")
            .unwrap();
        assert!(ls.get_all_aliases().any(|alias| alias == "list"));
    }

    #[tokio::test]
    async fn list_discovers_nested_sources_as_json() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("nested"))?;
        fs::write(
            temp.path().join("nested/trajectory.json"),
            r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
        )?;
        fs::write(
            temp.path().join("trajectory.jsonl"),
            r#"{"schema_version":"ATIF-v1.4","session_id":"s2","steps":[],"agent":{"id":"a"}}"#,
        )?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "ls",
            temp.path().to_str().unwrap(),
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["schema_version"], "pchronicle.ls.v1");
        assert_eq!(value["sources"].as_array().unwrap().len(), 2);
        assert_eq!(value["sources"][0]["source_path"], "nested/trajectory.json");
        assert_eq!(value["sources"][1]["source_path"], "trajectory.jsonl");
        assert!(String::from_utf8(stderr)?.contains("snapshot_id="));
        Ok(())
    }

    #[tokio::test]
    async fn list_alias_and_table_output_work() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("trajectory.json"), "[]")?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "list",
            temp.path().to_str().unwrap(),
            "--format",
            "table",
            "--physical",
        ])?;
        let mut stdout = Vec::new();
        run(cli, true, &mut stdout, &mut Vec::new()).await?;
        let output = String::from_utf8(stdout)?;
        assert!(output.contains("SOURCE"));
        assert!(output.contains("LAST MODIFIED"));
        assert!(output.contains("trajectory.json"));
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_exact_counts_as_json() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "status",
            atif_fixture().to_str().unwrap(),
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["schema_version"], "pchronicle.status.v1");
        assert_eq!(value["status"], "ready");
        assert_eq!(value["counts_complete"], true);
        assert_eq!(value["sources"]["total"], 1);
        assert_eq!(value["sources"]["ready"], 1);
        assert_eq!(value["sources"]["error"], 0);
        assert_eq!(value["counts"]["runs"], 1);
        assert_eq!(value["counts"]["trajectories"], 1);
        assert_eq!(value["counts"]["steps"], 10);
        assert_eq!(value["counts"]["tool_calls"], 0);
        assert_eq!(value["counts"]["events"], 0);
        assert!(value["source_errors"].as_array().unwrap().is_empty());
        assert!(String::from_utf8(stderr)?.contains("counts_complete=true"));
        Ok(())
    }

    #[tokio::test]
    async fn status_reports_partial_counts_for_bad_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::copy(atif_fixture(), temp.path().join("valid.json"))?;
        fs::write(temp.path().join("broken.json"), "{not-json")?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "status",
            temp.path().to_str().unwrap(),
            "--format",
            "json",
            "--errors",
            "report",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["status"], "degraded");
        assert_eq!(value["counts_complete"], false);
        assert_eq!(value["sources"]["total"], 2);
        assert_eq!(value["sources"]["ready"], 1);
        assert_eq!(value["sources"]["error"], 1);
        assert_eq!(value["counts"]["runs"], 1);
        assert_eq!(value["counts"]["steps"], 10);
        assert_eq!(value["source_errors"][0]["source_path"], "broken.json");
        Ok(())
    }

    #[tokio::test]
    async fn status_strict_mode_rejects_bad_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("broken.json"), "{not-json")?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "status",
            temp.path().to_str().unwrap(),
            "--errors",
            "strict",
        ])?;

        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test]
    async fn status_report_mode_marks_an_unreadable_dataset_as_error() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("broken.json"), "{not-json")?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "status",
            temp.path().to_str().unwrap(),
            "--format",
            "json",
            "--errors",
            "report",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["status"], "error");
        assert_eq!(value["sources"]["ready"], 0);
        assert_eq!(value["sources"]["error"], 1);
        let error = value["source_errors"][0]["error"].as_str().unwrap();
        assert!(error.contains("<dataset>/broken.json"));
        assert!(!error.contains(temp.path().to_str().unwrap()));
        Ok(())
    }

    #[tokio::test]
    async fn status_table_marks_counts_as_exact() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "status",
            atif_fixture().to_str().unwrap(),
            "--format",
            "table",
        ])?;
        let mut stdout = Vec::new();
        run(cli, true, &mut stdout, &mut Vec::new()).await?;

        let output = String::from_utf8(stdout)?;
        assert!(output.contains("FIELD"));
        assert!(output.contains("ACCURACY"));
        assert!(output.contains("trajectories  1          exact"));
        assert!(output.contains("steps         10          exact"));
        Ok(())
    }

    #[tokio::test]
    async fn status_rejects_zero_timeout() -> Result<()> {
        let cli = Cli::try_parse_from(["pchronicle", "status", ".", "--timeout-seconds", "0"])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "--timeout-seconds must be greater than zero"
        );
        Ok(())
    }

    #[tokio::test]
    async fn query_reads_all_example_dataset_formats_as_jsonl() -> Result<()> {
        for (format, expected_runs) in [("atif", 1), ("openai-messages", 2), ("actf", 1)] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "query",
                example_dataset(format).to_str().unwrap(),
                "SELECT COUNT(*) AS runs FROM dataset.runs",
                "--format",
                "jsonl",
            ])?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            run(cli, false, &mut stdout, &mut stderr).await?;

            let value: Value = serde_json::from_slice(&stdout)?;
            assert_eq!(value["runs"], expected_runs, "format={format}");
            assert!(String::from_utf8(stderr)?.contains("datasets=dataset"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn query_preserves_selected_column_order_in_table_and_csv() -> Result<()> {
        let dataset = example_dataset("atif");
        for format in ["table", "csv"] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "query",
                dataset.to_str().unwrap(),
                "SELECT session_id, step_id, source FROM dataset.steps ORDER BY step_id",
                "--format",
                format,
            ])?;
            let mut stdout = Vec::new();
            run(cli, format == "table", &mut stdout, &mut Vec::new()).await?;
            let output = String::from_utf8(stdout)?;
            let header = output.lines().next().unwrap();
            if format == "table" {
                assert_eq!(
                    header.split_whitespace().collect::<Vec<_>>(),
                    ["session_id", "step_id", "source"]
                );
            } else {
                assert_eq!(header, "session_id,step_id,source");
            }
            assert!(output.contains("support-001"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn query_supports_named_cross_dataset_sql() -> Result<()> {
        let atif = format!("atif={}", example_dataset("atif").display());
        let openai = format!("openai={}", example_dataset("openai-messages").display());
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            "--dataset",
            &atif,
            "--dataset",
            &openai,
            "SELECT (SELECT COUNT(*) FROM atif.runs) AS atif_runs, \
             (SELECT COUNT(*) FROM openai.runs) AS openai_runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["atif_runs"], 1);
        assert_eq!(value["openai_runs"], 2);
        Ok(())
    }

    #[tokio::test]
    async fn query_writes_new_files_without_overwriting() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let output = temp.path().join("runs.csv");
        let dataset = example_dataset("actf");
        let args = [
            "pchronicle",
            "query",
            dataset.to_str().unwrap(),
            "SELECT session_id FROM dataset.runs",
            "--format",
            "csv",
            "--output",
            output.to_str().unwrap(),
        ];
        let cli = Cli::try_parse_from(args)?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        assert!(stdout.is_empty());
        assert_eq!(
            fs::read_to_string(&output)?,
            "session_id\nexample-code-repair\n"
        );

        let cli = Cli::try_parse_from(args)?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("create query output file"));
        Ok(())
    }

    #[tokio::test]
    async fn query_rejects_writes_and_bounded_output_without_partial_stdout() -> Result<()> {
        for (sql, limit_flag, limit, expected) in [
            (
                "DELETE FROM dataset.runs",
                "--max-output-rows",
                "100",
                "only accepts SELECT",
            ),
            (
                "SELECT * FROM dataset.steps",
                "--max-output-rows",
                "1",
                "max_output_rows",
            ),
            (
                "SELECT * FROM dataset.steps",
                "--max-output-bytes",
                "8",
                "max_output_bytes",
            ),
        ] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "query",
                example_dataset("atif").to_str().unwrap(),
                sql,
                limit_flag,
                limit,
            ])?;
            let mut stdout = Vec::new();
            let error = run(cli, false, &mut stdout, &mut Vec::new())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(stdout.is_empty());
        }
        Ok(())
    }

    #[tokio::test]
    async fn query_errors_redact_sql_and_dataset_path() -> Result<()> {
        let dataset = example_dataset("atif");
        let sql = "SELECT secret_column FROM dataset.runs";
        let cli = Cli::try_parse_from(["pchronicle", "query", dataset.to_str().unwrap(), sql])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(!message.contains(sql));
        assert!(!message.contains(dataset.to_str().unwrap()));
        assert!(message.contains("<sql>"));
        Ok(())
    }

    #[tokio::test]
    async fn query_rejects_malformed_named_dataset_mounts() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            "--dataset",
            "missing-separator",
            "SELECT 1",
        ])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("NAME=URI"));
        Ok(())
    }

    #[tokio::test]
    async fn find_locates_runs_sessions_and_steps_in_example_datasets() -> Result<()> {
        for (format, flag, identity, expected_source) in [
            ("atif", "--session-id", "support-001", "support-ticket.json"),
            (
                "actf",
                "--run-id",
                "example-code-repair",
                "code-repair.actf.json",
            ),
            (
                "openai-messages",
                "--session-id",
                "training-002",
                "training.json",
            ),
        ] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "find",
                example_dataset(format).to_str().unwrap(),
                flag,
                identity,
                "--format",
                "json",
            ])?;
            let mut stdout = Vec::new();
            run(cli, false, &mut stdout, &mut Vec::new()).await?;

            let value: Value = serde_json::from_slice(&stdout)?;
            assert_eq!(value["schema_version"], "pchronicle.find.v1");
            assert_eq!(value["truncated"], false);
            assert_eq!(value["matches"][0]["source_path"], expected_source);
        }

        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--session-id",
            "support-001",
            "--step-id",
            "2",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["matches"][0]["step_id"], 2);
        assert_eq!(value["matches"][0]["step_source"], "agent");
        assert_eq!(value["matches"][0]["effective_kind"], "autonomous");
        Ok(())
    }

    #[tokio::test]
    async fn find_discovers_candidates_and_source_narrows_them() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for file in ["first.json", "second.json"] {
            fs::write(
                temp.path().join(file),
                r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
            )?;
        }

        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            temp.path().to_str().unwrap(),
            "--session-id",
            "shared",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["matches"].as_array().unwrap().len(), 2);

        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            temp.path().to_str().unwrap(),
            "--source",
            "second.json",
            "--session-id",
            "shared",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["matches"][0]["source_path"], "second.json");
        Ok(())
    }

    #[tokio::test]
    async fn find_reports_truncation_and_empty_results() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("openai-messages").to_str().unwrap(),
            "--run-id",
            "training-001",
            "--max-results",
            "1",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["truncated"], false);
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);

        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--session-id",
            "missing",
            "--format",
            "table",
        ])?;
        let mut stdout = Vec::new();
        run(cli, true, &mut stdout, &mut Vec::new()).await?;
        assert!(String::from_utf8(stdout)?.contains("(0 matches)"));
        Ok(())
    }

    #[tokio::test]
    async fn find_truncates_ambiguous_candidates() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for file in ["a.json", "b.json"] {
            fs::write(
                temp.path().join(file),
                r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
            )?;
        }
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            temp.path().to_str().unwrap(),
            "--session-id",
            "shared",
            "--max-results",
            "1",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["truncated"], true);
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn find_validates_source_paths_and_escapes_quotes() -> Result<()> {
        for source in ["/absolute.json", "../outside.json", "s3://bucket/file"] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "find",
                example_dataset("atif").to_str().unwrap(),
                "--source",
                source,
                "--session-id",
                "support-001",
            ])?;
            assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err());
        }

        let temp = tempfile::tempdir()?;
        fs::write(
            temp.path().join("it's-valid.json"),
            r#"[{"id":"event","session_id":"quoted","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
        )?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            temp.path().to_str().unwrap(),
            "--source",
            "it's-valid.json",
            "--session-id",
            "quoted",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["matches"][0]["source_path"], "it's-valid.json");
        Ok(())
    }

    #[tokio::test]
    async fn find_enforces_output_byte_limit_without_partial_stdout() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--session-id",
            "support-001",
            "--max-output-bytes",
            "8",
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        let error = run(cli, false, &mut stdout, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("max_output_bytes"), "{error:#}");
        assert!(stdout.is_empty());
        Ok(())
    }

    #[test]
    fn find_cli_requires_one_identity_and_session_for_steps() {
        assert!(Cli::try_parse_from(["pchronicle", "find", "."]).is_err());
        assert!(Cli::try_parse_from([
            "pchronicle",
            "find",
            ".",
            "--run-id",
            "r",
            "--session-id",
            "s"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--step-id", "1"]).is_err());
    }

    #[tokio::test]
    async fn find_rejects_empty_and_oversized_identities() -> Result<()> {
        for identity in ["", &"x".repeat(4097)] {
            let cli = Cli::try_parse_from([
                "pchronicle",
                "find",
                example_dataset("atif").to_str().unwrap(),
                "--session-id",
                identity,
            ])?;
            assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err());
        }
        Ok(())
    }

    #[test]
    fn rejects_credentials_and_signed_queries() {
        assert!(normalize_and_validate_dataset_uri("s3://user:secret@bucket/path").is_err());
        assert!(
            normalize_and_validate_dataset_uri("s3://bucket/path?X-Amz-Signature=secret").is_err()
        );
        assert!(normalize_and_validate_dataset_uri("https://example.com/data").is_err());
    }

    #[test]
    fn preserves_uri_roots_while_trimming_prefixes() {
        assert_eq!(
            normalize_and_validate_dataset_uri("file:///").unwrap(),
            "file:///"
        );
        assert_eq!(
            normalize_and_validate_dataset_uri("s3://bucket///").unwrap(),
            "s3://bucket"
        );
    }

    #[tokio::test]
    async fn placeholder_commands_fail_explicitly() -> Result<()> {
        let cli = Cli::try_parse_from(["pchronicle", "maintain", "."])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pchronicle maintain is not implemented yet"
        );
        Ok(())
    }

    #[test]
    fn byte_units_are_stable() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }
}
