use std::collections::HashSet;
use std::ffi::CString;
use std::fmt::Write as _;
use std::io::{Error as IoError, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use persisting_pchronicle::convert::storyline_to_atif;
use persisting_pchronicle::{
    actf_to_storylines, detect_format, load_atif_trajectories, parse_actf_document,
    parse_openai_msg_corpus_value, storylines_to_actf, CatalogErrorPolicy, CatalogSnapshotOptions,
    CatalogSourceKind, CatalogSourceStatus, CatalogStorylineKey, ChronicleFormat,
    ChronicleQueryEngine, DatasetCatalogSnapshot, DatasetMount, LocalQueryManifestOptions,
    StorylineDocument, DEFAULT_DATASET_NAME,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
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
    /// Input trajectory file, or - with --stream for stdin.
    #[arg(short = 'f', long = "from", value_name = "PATH_OR_STDIN")]
    from: String,

    /// New local Dataset directory. The target must not already exist.
    #[arg(short, long, value_name = "NEW_DATASET_URI")]
    output: String,

    /// Input exchange format. Auto detects regular files from name and content.
    #[arg(long, value_enum, default_value_t = ExchangeFormat::Auto)]
    format: ExchangeFormat,

    /// Read a finite trajectory stream from stdin and publish only after EOF.
    #[arg(long)]
    stream: bool,

    /// Reject inputs larger than this many bytes.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_input_bytes: usize,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Local path or object-store URI of the Dataset.
    #[arg(short = 'f', long = "from", value_name = "DATASET_URI")]
    from: String,

    /// New local file, or - for stdout.
    #[arg(short, long, value_name = "PATH_OR_STDOUT")]
    output: String,

    /// Output exchange format.
    #[arg(long, value_enum)]
    format: ExchangeFormat,

    /// Narrow export to one Dataset-relative Source.
    #[arg(long)]
    source: Option<String>,

    /// Export Trajectories with this Source-local Run ID.
    #[arg(long)]
    run_id: Option<String>,

    /// Export one Source-local Session ID.
    #[arg(long)]
    session_id: Option<String>,

    /// Additional SQL expression evaluated only against the trajectories view.
    #[arg(long, value_name = "EXPRESSION")]
    r#where: Option<String>,

    /// Refuse any export that cannot preserve the original exchange document.
    #[arg(long)]
    strict: bool,

    /// Atomically replace an existing local output file.
    #[arg(long)]
    overwrite: bool,

    /// Write the finite export document to stdout; requires --output -.
    #[arg(long)]
    stream: bool,

    /// Maximum number of complete Trajectories to export.
    #[arg(long, default_value_t = 10_000)]
    max_trajectories: u64,

    /// Reject encoded output larger than this many bytes.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_output_bytes: usize,

    /// Maximum time for address selection and Storyline loading.
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
struct ServeArgs {
    /// Static Warehouse configuration file.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,

    /// Loopback address for the read-only API and Web UI.
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,

    /// Open the Web UI in the system browser after the listener is ready.
    #[arg(long)]
    open: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarehouseFile {
    /// Reserved for a future rebuildable on-disk cache. The current server
    /// keeps acceleration state in memory and never writes to this path.
    #[serde(default)]
    cache_dir: Option<PathBuf>,
    #[serde(default)]
    default_dataset: Option<String>,
    datasets: Vec<WarehouseDataset>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarehouseDataset {
    name: String,
    uri: String,
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

#[derive(Debug, Serialize)]
struct ImportResponse {
    schema_version: &'static str,
    dataset_uri: String,
    source_path: String,
    format: String,
    trajectories: usize,
    input_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct ExportAddress {
    source_path: String,
    run_id: String,
    session_id: String,
}

pub async fn run(
    cli: Cli,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    run_with_stdin(
        cli,
        stdout_is_terminal,
        &mut std::io::empty(),
        stdout,
        stderr,
    )
    .await
}

pub async fn run_with_stdin(
    cli: Cli,
    stdout_is_terminal: bool,
    stdin: &mut dyn Read,
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
        Command::Import(args) => run_import(args, stdin, stdout, stderr).await,
        Command::Export(args) => run_export(args, stdout, stderr).await,
        Command::Maintain(args) => not_implemented("maintain", Some(&args.dataset_uri)),
        Command::Serve(args) => run_serve(args, stderr).await,
    }
}

fn not_implemented(command: &str, _dataset_uri: Option<&str>) -> Result<()> {
    bail!("pchronicle {command} is not implemented yet")
}

const MAX_WAREHOUSE_CONFIG_BYTES: u64 = 1024 * 1024;
const MAX_WAREHOUSE_DATASETS: usize = 128;

fn load_warehouse_config(
    path: &Path,
) -> Result<persisting_pchronicle_server::ChronicleServerConfig> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("read Warehouse config metadata {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "Warehouse config must be a regular file"
    );
    anyhow::ensure!(
        metadata.len() <= MAX_WAREHOUSE_CONFIG_BYTES,
        "Warehouse config exceeds the {} byte limit",
        MAX_WAREHOUSE_CONFIG_BYTES
    );
    let mut content = String::new();
    std::fs::File::open(path)
        .with_context(|| format!("open Warehouse config {}", path.display()))?
        .take(MAX_WAREHOUSE_CONFIG_BYTES + 1)
        .read_to_string(&mut content)
        .with_context(|| format!("read Warehouse config {}", path.display()))?;
    anyhow::ensure!(
        content.len() as u64 <= MAX_WAREHOUSE_CONFIG_BYTES,
        "Warehouse config exceeds the {} byte limit",
        MAX_WAREHOUSE_CONFIG_BYTES
    );
    let file: WarehouseFile = toml::from_str(&content)
        .with_context(|| format!("parse Warehouse config {}", path.display()))?;
    anyhow::ensure!(!file.datasets.is_empty(), "mount at least one Dataset");
    anyhow::ensure!(
        file.datasets.len() <= MAX_WAREHOUSE_DATASETS,
        "Warehouse config mounts more than {MAX_WAREHOUSE_DATASETS} Datasets"
    );

    if let Some(cache_dir) = &file.cache_dir {
        anyhow::ensure!(
            cache_dir.is_absolute(),
            "cache_dir must be an absolute path when configured"
        );
    }

    let mut names = HashSet::with_capacity(file.datasets.len());
    let mut mounts = Vec::with_capacity(file.datasets.len());
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    for dataset in file.datasets {
        let input = if !dataset.uri.contains("://") && Path::new(&dataset.uri).is_relative() {
            config_dir.join(&dataset.uri).to_string_lossy().into_owned()
        } else {
            dataset.uri
        };
        let uri = normalize_and_validate_dataset_uri(&input)
            .with_context(|| format!("validate Dataset '{}'", dataset.name))?;
        let mount = DatasetMount::new(dataset.name, uri)?;
        anyhow::ensure!(
            names.insert(mount.name.clone()),
            "Dataset names must be unique; duplicate '{}'",
            mount.name
        );
        mounts.push(mount);
    }

    let mut config = persisting_pchronicle_server::ChronicleServerConfig::mounted(mounts)?;
    if let Some(default_dataset) = file.default_dataset {
        let normalized = DatasetMount::new(default_dataset, "validation")?.name;
        anyhow::ensure!(
            names.contains(&normalized),
            "default_dataset '{normalized}' is not mounted"
        );
        config.default_dataset = Some(normalized);
    }
    config.catalog_options.error_policy = CatalogErrorPolicy::Report;
    Ok(config)
}

async fn run_serve(args: ServeArgs, stderr: &mut dyn Write) -> Result<()> {
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "pChronicle Warehouse may only bind to a loopback address"
    );
    let config = load_warehouse_config(&args.config)?;
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind pChronicle Warehouse to {}", args.listen))?;
    let addr = listener
        .local_addr()
        .context("read pChronicle Warehouse listen address")?;
    let url = format!("http://{addr}/");
    writeln!(stderr, "pChronicle Warehouse: {url}")
        .context("write pChronicle Warehouse address")?;
    if args.open {
        open_browser(&url)?;
    }
    persisting_pchronicle_server::serve_warehouse_with_listener(config, listener).await
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = ProcessCommand::new("open");
    #[cfg(target_os = "linux")]
    let mut command = ProcessCommand::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = ProcessCommand::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    bail!("--open is not supported on this platform; open {url} manually");

    command
        .arg(url)
        .spawn()
        .context("open pChronicle Warehouse in the system browser")?;
    Ok(())
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

async fn run_import(
    args: ImportArgs,
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
    let output = validate_new_local_dataset_path(&args.output)?;
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
    let trajectories = validate_import_source(format, &staged_source, text)?;
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

    let response = ImportResponse {
        schema_version: "pchronicle.import.v1",
        dataset_uri: output.to_string_lossy().into_owned(),
        source_path: source_path.into(),
        format: format.as_str().into(),
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

async fn run_export(
    args: ExportArgs,
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
    let (_, dataset_uris, snapshot) =
        discover_query_snapshot(Some(&args.from), &[], args.max_files, args.max_entries).await?;
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
    anyhow::ensure!(
        export.bytes.len() <= args.max_output_bytes,
        "encoded export exceeds max_output_bytes limit of {}",
        args.max_output_bytes
    );
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
    format: ChronicleFormat,
    dataset_uri: &str,
    snapshot: Arc<DatasetCatalogSnapshot>,
) -> Result<EncodedExport> {
    if let Some(export) = exact_local_file_export(args, format, dataset_uri, &snapshot)? {
        return Ok(export);
    }
    anyhow::ensure!(
        !args.strict,
        "strict export requires an unfiltered Source already stored in the requested format"
    );

    let sql = export_address_sql(args)?;
    let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone())
        .await
        .map_err(|error| redact_query_error(&error, &[dataset_uri.to_string()], None))?;
    let row_limit = args
        .max_trajectories
        .checked_add(1)
        .context("--max-trajectories is too large")?;
    let mut addresses = LimitedBuffer::new(args.max_output_bytes);
    engine
        .write_query_jsonl_with_max_rows(&sql, &mut addresses, Some(row_limit))
        .await
        .map_err(|error| redact_query_error(&error, &[dataset_uri.to_string()], Some(&sql)))?;
    let mut addresses = addresses
        .into_inner()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).context("decode export Trajectory address"))
        .collect::<Result<Vec<ExportAddress>>>()?;
    anyhow::ensure!(
        addresses.len() <= usize::try_from(args.max_trajectories).unwrap_or(usize::MAX),
        "export exceeds max_trajectories limit of {}",
        args.max_trajectories
    );
    anyhow::ensure!(
        !addresses.is_empty(),
        "export selection matched no Trajectories"
    );
    addresses.sort_by(|left, right| {
        (&left.source_path, &left.session_id, &left.run_id).cmp(&(
            &right.source_path,
            &right.session_id,
            &right.run_id,
        ))
    });
    let mut stories = Vec::with_capacity(addresses.len());
    let mut normalized_bytes = 0usize;
    for address in &addresses {
        let key = CatalogStorylineKey {
            dataset: DEFAULT_DATASET_NAME.into(),
            file: address.source_path.clone(),
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
            story.run_id.as_deref().unwrap_or(&story.session_id) == address.run_id,
            "export Trajectory Run ID changed within the snapshot"
        );
        normalized_bytes = normalized_bytes
            .checked_add(serde_json::to_vec(&story)?.len())
            .context("normalized export size overflow")?;
        anyhow::ensure!(
            normalized_bytes <= args.max_output_bytes,
            "normalized export exceeds max_output_bytes limit of {}",
            args.max_output_bytes
        );
        stories.push(story);
    }
    let bytes = encode_export(format, &stories)?;
    Ok(EncodedExport {
        bytes,
        trajectories: stories.len(),
        exact: false,
    })
}

fn exact_local_file_export(
    args: &ExportArgs,
    format: ChronicleFormat,
    dataset_uri: &str,
    snapshot: &DatasetCatalogSnapshot,
) -> Result<Option<EncodedExport>> {
    if args.run_id.is_some() || args.session_id.is_some() || args.r#where.is_some() {
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
    anyhow::ensure!(
        input.len() <= args.max_output_bytes,
        "exact export exceeds max_output_bytes limit of {}",
        args.max_output_bytes
    );
    let text = std::str::from_utf8(&input).context("exact export Source must be UTF-8")?;
    let detected = detect_format(Some(&source_path), Some(text)).map_err(anyhow::Error::from)?;
    if detected != Some(format) {
        return Ok(None);
    }
    let trajectories = validate_import_source(format, &source_path, text)?;
    anyhow::ensure!(
        sources[0].size_bytes == Some(input.len() as u64)
            && sources[0].snapshot_ref.as_deref() == Some(&local_file_snapshot_ref(&source_path)),
        "export Source changed after the Catalog Snapshot was created"
    );
    Ok(Some(EncodedExport {
        bytes: input,
        trajectories,
        exact: true,
    }))
}

fn export_address_sql(args: &ExportArgs) -> Result<String> {
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
        "SELECT _file_ AS source_path, run_id, session_id \
         FROM dataset.trajectories{predicate} \
         ORDER BY _file_, session_id, run_id LIMIT {limit}"
    ))
}

fn encode_export(format: ChronicleFormat, stories: &[StorylineDocument]) -> Result<Vec<u8>> {
    let value = match format {
        ChronicleFormat::Atif => {
            let documents = stories
                .iter()
                .map(storyline_to_atif)
                .collect::<persisting_pchronicle::Result<Vec<_>>>()?;
            if documents.len() == 1 {
                serde_json::to_value(&documents[0])?
            } else {
                serde_json::to_value(documents)?
            }
        }
        ChronicleFormat::Actf => serde_json::to_value(storylines_to_actf(stories)?)?,
        ChronicleFormat::OpenaiMsg => encode_openai_export(stories)?,
        ChronicleFormat::Storyline => {
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

fn encode_openai_export(stories: &[StorylineDocument]) -> Result<serde_json::Value> {
    if let Ok(files) = persisting_pchronicle::recover_openai_msg_files(stories) {
        anyhow::ensure!(
            files.len() == 1,
            "one export document cannot preserve {} OpenAI source files; select one Source",
            files.len()
        );
        return Ok(files.into_iter().next().expect("one file checked").document);
    }
    let mut records = Vec::new();
    for story in stories {
        let document = persisting_pchronicle::from_storyline(ChronicleFormat::OpenaiMsg, story)?;
        let document: serde_json::Value = serde_json::from_str(&document)?;
        records.extend(
            document
                .get("session_steps")
                .and_then(serde_json::Value::as_array)
                .context("synthesized OpenAI export has no session_steps array")?
                .iter()
                .cloned(),
        );
    }
    Ok(serde_json::Value::Array(records))
}

fn export_format(format: ExchangeFormat) -> Result<ChronicleFormat> {
    Ok(match format {
        ExchangeFormat::Auto => bail!("export requires an explicit --format"),
        ExchangeFormat::Atif => ChronicleFormat::Atif,
        ExchangeFormat::Actf => ChronicleFormat::Actf,
        ExchangeFormat::OpenaiMessages => ChronicleFormat::OpenaiMsg,
        ExchangeFormat::Storyline => ChronicleFormat::Storyline,
    })
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
        .context("--max-input-bytes is too large")?;
    let mut input = Vec::new();
    reader
        .by_ref()
        .take(limit)
        .read_to_end(&mut input)
        .with_context(|| format!("read {label}"))?;
    anyhow::ensure!(
        input.len() <= max_bytes,
        "{label} exceeds max_input_bytes limit of {max_bytes}"
    );
    anyhow::ensure!(!input.is_empty(), "{label} is empty");
    Ok(input)
}

fn resolve_import_format(
    requested: ExchangeFormat,
    input_path: Option<&Path>,
    input: &str,
) -> Result<ChronicleFormat> {
    let format = match requested {
        ExchangeFormat::Auto => detect_format(input_path, Some(input))
            .map_err(anyhow::Error::from)?
            .context("cannot detect import format; pass --format explicitly")?,
        ExchangeFormat::Atif => ChronicleFormat::Atif,
        ExchangeFormat::Actf => ChronicleFormat::Actf,
        ExchangeFormat::OpenaiMessages => ChronicleFormat::OpenaiMsg,
        ExchangeFormat::Storyline => ChronicleFormat::Storyline,
    };
    anyhow::ensure!(
        matches!(
            format,
            ChronicleFormat::Atif | ChronicleFormat::Actf | ChronicleFormat::OpenaiMsg
        ),
        "import format '{format}' is not supported by the first queryable import increment"
    );
    Ok(format)
}

fn import_source_name(format: ChronicleFormat) -> &'static str {
    match format {
        ChronicleFormat::Atif => "trajectories.atif.json",
        ChronicleFormat::Actf => "trajectories.actf.json",
        ChronicleFormat::OpenaiMsg => "session_steps.json",
        _ => unreachable!("unsupported import format was rejected"),
    }
}

fn validate_import_source(format: ChronicleFormat, path: &Path, input: &str) -> Result<usize> {
    match format {
        ChronicleFormat::Atif => {
            let trajectories = load_atif_trajectories(path)?;
            ensure_unique_session_ids(
                trajectories
                    .iter()
                    .map(|trajectory| trajectory.effective_session_id())
                    .collect::<persisting_pchronicle::Result<Vec<_>>>()?,
            )?;
            Ok(trajectories.len())
        }
        ChronicleFormat::OpenaiMsg => {
            let document = serde_json::from_str(input).context("parse OpenAI Messages JSON")?;
            let stories = parse_openai_msg_corpus_value(&document, "session_steps.json")?;
            ensure_unique_session_ids(stories.iter().map(|story| story.session_id.as_str()))?;
            Ok(stories.len())
        }
        ChronicleFormat::Actf => {
            let document = parse_actf_document(input).map_err(anyhow::Error::from)?;
            let stories = actf_to_storylines(&document).map_err(anyhow::Error::from)?;
            ensure_unique_session_ids(stories.iter().map(|story| story.session_id.as_str()))?;
            Ok(stories.len())
        }
        _ => unreachable!("unsupported import format was rejected"),
    }
}

fn ensure_unique_session_ids<'a>(session_ids: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let mut seen = HashSet::new();
    for session_id in session_ids {
        anyhow::ensure!(
            seen.insert(session_id),
            "duplicate session_id: {session_id}"
        );
    }
    Ok(())
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
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
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
fn rename_noreplace(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic create-only Dataset publish is unsupported on this platform",
    ))
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

    fn example_source(format: &str) -> PathBuf {
        let filename = match format {
            "atif" => "support-ticket.json",
            "openai-messages" => "training.json",
            "actf" => "code-repair.actf.json",
            other => panic!("unknown example format: {other}"),
        };
        example_dataset(format).join(filename)
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

    #[tokio::test]
    async fn import_creates_queryable_lossless_datasets_for_all_example_formats() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for (format, expected_format, source_name, expected_runs) in [
            ("atif", "atif", "trajectories.atif.json", 1),
            ("openai-messages", "openai_msg", "session_steps.json", 2),
            ("actf", "actf", "trajectories.actf.json", 1),
        ] {
            let input = example_source(format);
            let output = temp.path().join(format);
            let cli = Cli::try_parse_from([
                "pchronicle",
                "import",
                "--from",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            run(cli, false, &mut stdout, &mut stderr).await?;

            let response: Value = serde_json::from_slice(&stdout)?;
            assert_eq!(response["schema_version"], "pchronicle.import.v1");
            assert_eq!(response["format"], expected_format);
            assert_eq!(response["source_path"], source_name);
            assert_eq!(response["trajectories"], expected_runs);
            assert_eq!(
                fs::read(output.join(source_name))?,
                fs::read(&input)?,
                "import must preserve the exchange document byte-for-byte"
            );
            assert!(String::from_utf8(stderr)?.contains("trajectories="));

            let cli = Cli::try_parse_from([
                "pchronicle",
                "query",
                output.to_str().unwrap(),
                "SELECT COUNT(*) AS runs FROM dataset.runs",
                "--format",
                "jsonl",
            ])?;
            let mut stdout = Vec::new();
            run(cli, false, &mut stdout, &mut Vec::new()).await?;
            let count: Value = serde_json::from_slice(&stdout)?;
            assert_eq!(count["runs"], expected_runs, "format={format}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn import_reads_a_bounded_explicit_stdin_stream() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let output = temp.path().join("streamed");
        let input = fs::read(example_source("atif"))?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            "-",
            "--stream",
            "--format",
            "atif",
            "--output",
            output.to_str().unwrap(),
            "--max-input-bytes",
            &input.len().to_string(),
        ])?;
        let mut stdin = input.as_slice();
        let mut stdout = Vec::new();
        run_with_stdin(cli, false, &mut stdin, &mut stdout, &mut Vec::new()).await?;
        let response: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(response["trajectories"], 1);
        assert_eq!(fs::read(output.join("trajectories.atif.json"))?, input);

        for args in [
            vec![
                "pchronicle",
                "import",
                "--from",
                "-",
                "--output",
                temp.path().join("missing-stream").to_str().unwrap(),
                "--format",
                "atif",
            ],
            vec![
                "pchronicle",
                "import",
                "--from",
                "-",
                "--stream",
                "--output",
                temp.path().join("missing-format").to_str().unwrap(),
            ],
        ] {
            let cli = Cli::try_parse_from(args)?;
            let mut stdin = input.as_slice();
            assert!(
                run_with_stdin(cli, false, &mut stdin, &mut Vec::new(), &mut Vec::new())
                    .await
                    .is_err()
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn import_rejects_invalid_oversized_and_unsupported_input_without_partial_output(
    ) -> Result<()> {
        let temp = tempfile::tempdir()?;
        let invalid = temp.path().join("invalid.json");
        fs::write(&invalid, "not json")?;

        for (name, extra) in [
            ("invalid", vec![]),
            ("oversized", vec!["--max-input-bytes", "1"]),
            ("storyline", vec!["--format", "storyline"]),
        ] {
            let output = temp.path().join(name);
            let mut args = vec![
                "pchronicle",
                "import",
                "--from",
                invalid.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ];
            args.extend(extra);
            let cli = Cli::try_parse_from(args)?;
            let mut stdout = Vec::new();
            assert!(run(cli, false, &mut stdout, &mut Vec::new()).await.is_err());
            assert!(stdout.is_empty());
            assert!(!output.exists());
        }
        assert!(!fs::read_dir(temp.path())?.any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.starts_with(".pchronicle-import-"))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn import_is_create_only_and_rejects_duplicate_sessions() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let output = temp.path().join("existing");
        fs::create_dir(&output)?;
        fs::write(output.join("sentinel"), "keep")?;
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            example_source("atif").to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])?;
        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
        assert_eq!(fs::read_to_string(output.join("sentinel"))?, "keep");

        let trajectory: Value = serde_json::from_slice(&fs::read(example_source("atif"))?)?;
        let duplicate_input = temp.path().join("duplicates.json");
        fs::write(
            &duplicate_input,
            serde_json::to_vec(&serde_json::json!([trajectory.clone(), trajectory]))?,
        )?;
        let duplicate_output = temp.path().join("duplicates");
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            duplicate_input.to_str().unwrap(),
            "--output",
            duplicate_output.to_str().unwrap(),
            "--format",
            "atif",
        ])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("duplicate session_id"),
            "{error:#}"
        );
        assert!(!duplicate_output.exists());
        Ok(())
    }

    #[test]
    fn import_publish_primitive_never_replaces_an_existing_target() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let staged = temp.path().join("staged");
        let existing = temp.path().join("existing");
        fs::create_dir(&staged)?;
        fs::create_dir(&existing)?;
        fs::write(staged.join("new"), "new")?;
        fs::write(existing.join("sentinel"), "keep")?;

        assert!(rename_noreplace(&staged, &existing).is_err());
        assert_eq!(fs::read_to_string(existing.join("sentinel"))?, "keep");
        assert_eq!(fs::read_to_string(staged.join("new"))?, "new");
        Ok(())
    }

    #[tokio::test]
    async fn export_filters_complete_trajectories_and_streams_finite_json() -> Result<()> {
        let dataset = example_dataset("openai-messages");
        let cli = Cli::try_parse_from([
            "pchronicle",
            "export",
            "--from",
            dataset.to_str().unwrap(),
            "--output",
            "-",
            "--stream",
            "--format",
            "openai-messages",
            "--session-id",
            "training-002",
            "--where",
            "step_count = 2",
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let rows: Value = serde_json::from_slice(&stdout)?;
        let rows = rows.as_array().context("OpenAI export must be an array")?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["session_id"], "training-002");
        assert!(String::from_utf8(stderr)?.contains("trajectories=1"));
        Ok(())
    }

    #[tokio::test]
    async fn export_converts_complete_trajectories_between_formats() -> Result<()> {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--output",
            "-",
            "--stream",
            "--format",
            "storyline",
            "--session-id",
            "support-001",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let story: persisting_pchronicle::StorylineDocument = serde_json::from_slice(&stdout)?;
        assert_eq!(story.session_id, "support-001");
        assert_eq!(story.turns.len(), 3);
        Ok(())
    }

    #[tokio::test]
    async fn export_is_bounded_create_only_and_has_no_partial_output() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let output = temp.path().join("export.json");
        let dataset = example_dataset("atif");
        fs::write(&output, "sentinel")?;
        let base = [
            "pchronicle",
            "export",
            "--from",
            dataset.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--format",
            "atif",
        ];
        let cli = Cli::try_parse_from(base)?;
        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
        assert_eq!(fs::read_to_string(&output)?, "sentinel");

        let mut overwrite = base.to_vec();
        overwrite.push("--overwrite");
        let cli = Cli::try_parse_from(overwrite)?;
        run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
        assert!(fs::read_to_string(&output)?.contains("support-001"));

        let limited = temp.path().join("limited.json");
        let cli = Cli::try_parse_from([
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--output",
            limited.to_str().unwrap(),
            "--format",
            "atif",
            "--max-output-bytes",
            "8",
        ])?;
        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
        assert!(!limited.exists());
        assert!(!fs::read_dir(temp.path())?.any(|entry| {
            entry
                .ok()
                .and_then(|entry| entry.file_name().into_string().ok())
                .is_some_and(|name| name.starts_with(".pchronicle-export-"))
        }));
        Ok(())
    }

    #[tokio::test]
    async fn export_validates_stream_filters_and_strict_conversion() -> Result<()> {
        for args in [
            vec![
                "pchronicle",
                "export",
                "--from",
                example_dataset("atif").to_str().unwrap(),
                "--output",
                "-",
                "--format",
                "atif",
            ],
            vec![
                "pchronicle",
                "export",
                "--from",
                example_dataset("atif").to_str().unwrap(),
                "--output",
                "-",
                "--stream",
                "--format",
                "storyline",
                "--strict",
            ],
            vec![
                "pchronicle",
                "export",
                "--from",
                example_dataset("atif").to_str().unwrap(),
                "--output",
                "-",
                "--stream",
                "--format",
                "atif",
                "--where",
                "DELETE FROM dataset.runs",
            ],
        ] {
            let cli = Cli::try_parse_from(args)?;
            let mut stdout = Vec::new();
            assert!(run(cli, false, &mut stdout, &mut Vec::new()).await.is_err());
            assert!(stdout.is_empty());
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
    fn warehouse_config_normalizes_mounts_and_selects_default() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first)?;
        fs::create_dir_all(&second)?;
        let config_path = temp.path().join("warehouse.toml");
        fs::write(
            &config_path,
            format!(
                r#"
cache_dir = "/tmp/pchronicle-cache"
default_dataset = "archive"

[[datasets]]
name = "live"
uri = {first:?}

[[datasets]]
name = "archive"
uri = {second:?}
"#,
                first = first.to_string_lossy(),
                second = second.to_string_lossy(),
            ),
        )?;

        let config = load_warehouse_config(&config_path)?;
        assert_eq!(config.datasets.len(), 2);
        assert_eq!(config.default_dataset.as_deref(), Some("archive"));
        assert_eq!(config.writable_dataset, None);
        assert_eq!(
            config.catalog_options.error_policy,
            CatalogErrorPolicy::Report
        );
        assert_eq!(
            config.datasets[0].uri,
            fs::canonicalize(first)?.to_string_lossy()
        );
        assert_eq!(
            config.datasets[1].uri,
            fs::canonicalize(second)?.to_string_lossy()
        );
        Ok(())
    }

    #[test]
    fn warehouse_config_rejects_unsafe_or_ambiguous_mounts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let dataset = temp.path().join("dataset");
        fs::create_dir(&dataset)?;

        for (name, body, expected) in [
            (
                "duplicate.toml",
                format!(
                    "[[datasets]]\nname='live'\nuri={dataset:?}\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "unique",
            ),
            (
                "missing-default.toml",
                format!(
                    "default_dataset='missing'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "not mounted",
            ),
            (
                "credential.toml",
                "[[datasets]]\nname='live'\nuri='s3://user:secret@bucket/path'\n".into(),
                "credentials",
            ),
            (
                "unknown.toml",
                format!(
                    "listen='0.0.0.0:80'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "unknown field",
            ),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, body)?;
            let error = load_warehouse_config(&path).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "unexpected error for {name}: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn serve_cli_defaults_to_loopback_and_rejects_public_listeners() -> Result<()> {
        let cli = Cli::try_parse_from(["pchronicle", "serve", "--config", "warehouse.toml"])?;
        let Command::Serve(args) = cli.command else {
            unreachable!("serve command parsed as another variant")
        };
        assert_eq!(args.listen, "127.0.0.1:8080".parse::<SocketAddr>()?);

        let cli = Cli::try_parse_from([
            "pchronicle",
            "serve",
            "--config",
            "warehouse.toml",
            "--listen",
            "0.0.0.0:8080",
        ])?;
        let Command::Serve(args) = cli.command else {
            unreachable!("serve command parsed as another variant")
        };
        assert!(!args.listen.ip().is_loopback());
        Ok(())
    }

    #[test]
    fn byte_units_are_stable() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }
}
