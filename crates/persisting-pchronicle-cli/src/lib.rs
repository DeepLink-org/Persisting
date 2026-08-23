mod agent;
mod control;
mod exchange;
mod gateway_capture;
mod onboard;
mod output;
mod projection_supervisor;
pub mod server;
mod settings;

#[cfg(test)]
use exchange::rename_noreplace;
use exchange::{run_export, run_import};
use output::*;
use settings::*;

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
use futures::{stream, stream::FuturesUnordered, StreamExt};
use persisting_events::{ChronicleServeReady, CHRONICLE_SERVE_READY_VERSION};
use persisting_pchronicle::document::{
    decode_json_storylines, detect_format, encode_json_storylines, open_document, DocumentFormat,
    InputIssue, InputIssueKind,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::query::ChronicleQueryEngine;
use persisting_pchronicle::storage::{
    automatic_projection_inventory, build_storyline_projection,
    inspect_automatic_storyline_projection, probe_canonical_event_store,
    storyline_projection_destination_exists, AutomaticProjectionInspection,
    AutomaticProjectionState, CatalogErrorPolicy, CatalogSnapshotOptions, CatalogSourceKind,
    CatalogSourceStatus, CatalogStorylineKey, DatasetCatalogSnapshot, DatasetMount,
    DiscoveredSource, EventFactSnapshot, ObjectStoreManifestWriteMode, StorylineLanceStore,
    StorylineProjectionBuildOutcome, DEFAULT_DATASET_NAME,
};
use serde::{Deserialize, Serialize};
use url::Url;

use server::problem::BoundaryCode;

#[derive(Debug)]
struct CliBoundaryError {
    code: BoundaryCode,
    message: String,
}

impl std::fmt::Display for CliBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CliBoundaryError {}

fn cli_boundary_error(code: BoundaryCode, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CliBoundaryError {
        code,
        message: message.into(),
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "pchronicle",
    version,
    about = "Learn, browse, query, and exchange Agent trajectory Datasets"
)]
pub struct Cli {
    /// Override the pChronicle settings file (primarily for isolated environments).
    #[arg(long, global = true, value_name = "FILE")]
    settings: Option<PathBuf>,

    /// Show complete error source chains instead of concise errors.
    #[arg(long, global = true)]
    debug_errors: bool,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    pub fn debug_errors(&self) -> bool {
        self.debug_errors
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Learn the core pChronicle workflow with a guided Dataset walkthrough.
    Onboard(onboard::OnboardArgs),
    /// Show or set the local default Warehouse directory.
    Default(DefaultArgs),
    /// List trajectory Sources discovered under a Dataset URI.
    #[command(visible_alias = "list")]
    Ls(ListArgs),
    /// Show Dataset health and aggregate statistics.
    Status(StatusArgs),
    /// Execute read-only SQL over one or more Datasets.
    Query(QueryArgs),
    /// Run a stable built-in analysis over normalized trajectory tables.
    Analysis(AnalysisArgs),
    /// Start Codex or Claude with a pChronicle Dataset analysis skill.
    Agent(agent::AgentArgs),
    /// Locate a Run, Trajectory, or Step by its Source-local ID.
    Find(FindArgs),
    /// Create a new Dataset from one or more trajectory Sources.
    Import(ImportArgs),
    /// Export complete Trajectories to an exchange format.
    Export(ExportArgs),
    /// Run a deterministic local LLM upstream for Gateway testing.
    Echo(EchoArgs),
    /// Run explicitly enabled Warehouse, Control, and Gateway services.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Local path or object-store URI. Uses the default Warehouse when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

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
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
struct DefaultArgs {
    /// Local directory to use as the default Warehouse; created when absent.
    #[arg(value_name = "DIRECTORY")]
    directory: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Local path or object-store URI. Uses the default Warehouse when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Fail on a bad Source, or report partial counts and continue.
    #[arg(long, value_enum, default_value_t = ErrorMode::Report)]
    errors: ErrorMode,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
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
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
struct AnalysisArgs {
    #[command(subcommand)]
    command: AnalysisCommand,
}

#[derive(Debug, Subcommand)]
enum AnalysisCommand {
    /// Summarize Sources, trajectories, Steps, Agents, Models, and tools.
    #[command(visible_alias = "summary")]
    Overview(AnalysisOptions),
    /// Aggregate trajectory activity by Agent identity and version.
    Agents(AnalysisOptions),
    /// Aggregate declared and observed Model usage.
    Models(AnalysisOptions),
    /// Aggregate tool usage and duration coverage.
    #[command(visible_aliases = ["tool-calls", "toolcalls"])]
    Tools(AnalysisOptions),
}

#[derive(Debug, Args)]
struct AnalysisOptions {
    /// Local path or object-store URI. Uses the default Warehouse when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSONL when piped.
    #[arg(long, value_enum, default_value_t = QueryOutputFormat::Auto)]
    format: QueryOutputFormat,

    /// Maximum number of grouped rows returned.
    #[arg(long, default_value_t = 100)]
    limit: u64,

    /// Reject encoded results larger than this many bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024)]
    max_output_bytes: usize,

    /// Maximum time for analysis execution and result encoding.
    #[arg(long, default_value_t = 30)]
    timeout_seconds: u64,

    /// Maximum number of trajectory Sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("identity")
        .required(true)
        .multiple(false)
        .args(["document_id", "run_id", "session_id"])
))]
struct FindArgs {
    /// Local path or object-store URI. Uses the default Warehouse when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Narrow the lookup to one Dataset-relative Source path.
    #[arg(long)]
    source: Option<String>,

    /// Find Run or Trajectory candidates by Source-local Run ID.
    #[arg(long)]
    run_id: Option<String>,

    /// Find one trajectory by its stable Source-local document ID.
    #[arg(long)]
    document_id: Option<String>,

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
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
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

impl ExchangeFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Atif => "atif",
            Self::Actf => "actf",
            Self::OpenaiMessages => "openai-messages",
            Self::Storyline => "storyline",
        }
    }
}

impl std::fmt::Display for ExchangeFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ImportOutputFormat {
    /// Preserve each source document byte-for-byte.
    Preserve,
    /// Decode all input Sources into one squashed Storyline Lance Store at the Dataset root.
    Storyline,
}

impl ImportOutputFormat {
    fn response_name(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Storyline => "storyline-lance",
        }
    }
}

#[derive(Debug, Args)]
struct ImportArgs {
    /// Input trajectory file or directory, or - with --stream for stdin.
    #[arg(short = 'f', long = "from", value_name = "PATH_OR_STDIN")]
    from: String,

    /// New local Dataset directory. Defaults to a child of the default Warehouse.
    #[arg(short, long, value_name = "NEW_DATASET_URI")]
    output: Option<String>,

    /// Input exchange format. Auto detects each regular file from name and content.
    /// Directory imports skip JSON that is not a known trajectory format.
    #[arg(long, value_enum, default_value_t = ExchangeFormat::Auto)]
    format: ExchangeFormat,

    /// Physical Dataset output: preserve source files, or squash into one Storyline Lance Store at the Dataset root.
    #[arg(long, value_enum)]
    output_format: Option<ImportOutputFormat>,

    /// Read a finite trajectory stream from stdin and publish only after EOF.
    #[arg(long)]
    stream: bool,

    /// Optional per-Source byte limit. When omitted, input size is unbounded.
    #[arg(long)]
    max_input_bytes: Option<usize>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Local path or object-store URI. Uses the default Warehouse when omitted.
    #[arg(short = 'f', long = "from", value_name = "DATASET_URI")]
    from: Option<String>,

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

    /// Export one Source-local document ID.
    #[arg(long)]
    document_id: Option<String>,

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
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
#[command(
    group(
        ArgGroup::new("dataset_source")
            .required(true)
            .args(["config", "storage"])
    ),
    group(
        ArgGroup::new("serve_component")
            .required(true)
            .multiple(true)
            .args(["listen", "control", "gateway"])
    )
)]
struct ServeArgs {
    /// Static Warehouse configuration file.
    #[arg(long, value_name = "FILE", conflicts_with = "storage")]
    config: Option<PathBuf>,

    /// Dataset URI and durable Control root. Repeatable; NAME=URI overrides the derived name.
    #[arg(long, value_name = "URI", conflicts_with = "config")]
    storage: Vec<String>,

    /// Loopback address for the read-only API and Web UI.
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Loopback address for the authenticated Control protocol.
    #[arg(long, requires = "storage", conflicts_with = "config")]
    control: Option<SocketAddr>,

    /// Open the Web UI in the system browser after the listener is ready.
    #[arg(long, requires = "listen")]
    open: bool,

    /// Enable the LLM Gateway with an existing Gateway TOML configuration.
    #[arg(long, visible_alias = "gateway-config", value_name = "FILE")]
    gateway: Option<PathBuf>,

    /// Mounted Dataset that receives Gateway capture events.
    #[arg(long, value_name = "NAME", requires = "gateway")]
    gateway_dataset: Option<String>,

    /// Local Gateway state directory; required for an object-store Dataset.
    #[arg(long, value_name = "DIRECTORY", requires = "gateway")]
    gateway_state: Option<PathBuf>,

    /// Object-store manifest publication contract used by Gateway capture.
    #[arg(
        long,
        value_enum,
        default_value_t,
        requires = "gateway",
        value_name = "MODE"
    )]
    gateway_object_store_manifest_mode: GatewayObjectStoreManifestMode,

    /// Also maintain Gateway's live AgenticMD projection.
    #[arg(long, requires = "gateway")]
    gateway_stream_markdown: bool,

    /// Print Gateway diagnostics, including bounded request/response bodies, to stderr.
    #[arg(long, visible_alias = "gateway-debug", requires = "gateway")]
    debug: bool,
}

#[derive(Debug, Args)]
struct EchoArgs {
    /// Loopback address for the Echo server.
    #[arg(long, default_value = "127.0.0.1:19080")]
    listen: SocketAddr,

    /// Encode echoed text directly or as Base64.
    #[arg(long, value_enum, default_value_t = EchoEncoding::Plain)]
    encoding: EchoEncoding,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum EchoEncoding {
    #[default]
    Plain,
    Base64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum GatewayObjectStoreManifestMode {
    #[default]
    Conditional,
    /// One Gateway process owns the Dataset; conditional object replacement is unavailable.
    SingleWriter,
}

impl From<GatewayObjectStoreManifestMode> for ObjectStoreManifestWriteMode {
    fn from(mode: GatewayObjectStoreManifestMode) -> Self {
        match mode {
            GatewayObjectStoreManifestMode::Conditional => Self::Conditional,
            GatewayObjectStoreManifestMode::SingleWriter => Self::SingleWriter,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarehouseFile {
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
    dataset_uri: String,
    snapshot_id: String,
    created_at: String,
    status: &'static str,
    counts_complete: bool,
    sources: StatusSources,
    counts: StatusCounts,
    projections: Vec<ProjectionStatusResponse>,
    source_errors: Vec<StatusSourceError>,
}

#[derive(Debug, Serialize)]
struct ProjectionStatusResponse {
    source_path: String,
    projection_path: String,
    status: ProjectionStatusName,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_rows: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProjectionStatusName {
    Fresh,
    Stale,
    Missing,
    Error,
}

impl ProjectionStatusName {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
            Self::Missing => "missing",
            Self::Error => "error",
        }
    }
}

impl ProjectionStatusResponse {
    fn from_inspection(
        source_path: String,
        projection_path: String,
        inspection: AutomaticProjectionInspection,
    ) -> Self {
        Self {
            source_path,
            projection_path,
            status: match inspection.state {
                AutomaticProjectionState::Fresh => ProjectionStatusName::Fresh,
                AutomaticProjectionState::Stale => ProjectionStatusName::Stale,
                AutomaticProjectionState::Missing => ProjectionStatusName::Missing,
            },
            generation: inspection.generation,
            fact_version: Some(inspection.fact_version),
            fact_rows: Some(inspection.fact_rows),
        }
    }

    fn error(source_path: String, projection_path: String) -> Self {
        Self {
            source_path,
            projection_path,
            status: ProjectionStatusName::Error,
            generation: None,
            fact_version: None,
            fact_rows: None,
        }
    }
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

const STATUS_PROJECTION_CONCURRENCY: usize = 16;

#[derive(Debug, Serialize)]
struct FindResponse {
    dataset_uri: String,
    snapshot_id: String,
    query: FindQueryResponse,
    truncated: bool,
    matches: Vec<FindMatch>,
}

#[derive(Debug, Serialize)]
struct FindQueryResponse {
    source: Option<String>,
    document_id: Option<String>,
    run_id: Option<String>,
    session_id: Option<String>,
    step_id: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct FindMatch {
    source_path: String,
    document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
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
    dataset_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    output_format: String,
    sources: usize,
    trajectories: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    fact_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ExportAddress {
    source_path: String,
    document_id: String,
    run_id: Option<String>,
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
    run_with_stdio(cli, false, stdout_is_terminal, stdin, stdout, stderr).await
}

pub async fn run_with_stdio(
    cli: Cli,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let settings = cli.settings.as_deref();
    match cli.command {
        Command::Onboard(args) => {
            onboard::run(args, stdin_is_terminal, stdout_is_terminal, stdin, stdout).await
        }
        Command::Default(args) => run_default(args, settings, stdout, stderr),
        Command::Ls(args) => run_list(args, settings, stdout_is_terminal, stdout, stderr).await,
        Command::Status(args) => {
            run_status(args, settings, stdout_is_terminal, stdout, stderr).await
        }
        Command::Query(args) => run_query(args, settings, stdout_is_terminal, stdout, stderr).await,
        Command::Analysis(args) => {
            run_analysis(args, settings, stdout_is_terminal, stdout, stderr).await
        }
        Command::Agent(args) => agent::run(
            args,
            settings,
            stdin_is_terminal,
            stdout_is_terminal,
            stdout,
            stderr,
        ),
        Command::Find(args) => run_find(args, settings, stdout_is_terminal, stdout, stderr).await,
        Command::Import(args) => run_import(args, settings, stdin, stdout, stderr).await,
        Command::Export(args) => run_export(args, settings, stdout, stderr).await,
        Command::Echo(args) => run_echo(args, stderr).await,
        Command::Serve(args) => run_serve(args, stdout, stderr).await,
    }
}

struct PreparedGateway {
    config: persisting_gateway::config::ProxyConfig,
    state_dir: PathBuf,
    dataset_name: String,
    stream_markdown: bool,
    listener: tokio::net::TcpListener,
    admin_listener: tokio::net::TcpListener,
    sink: Arc<dyn persisting_gateway::sink::CaptureEventSink>,
    writer: gateway_capture::GatewayCaptureWriter,
}

const SERVE_STORAGE_DATASET_NAME: &str = "default";

fn select_gateway_dataset(
    config: &server::ChronicleServerConfig,
    requested: Option<&str>,
) -> Result<DatasetMount> {
    let name = match requested {
        Some(name) => DatasetMount::new(name, "validation")?.name,
        None => config
            .default_dataset
            .clone()
            .or_else(|| (config.datasets.len() == 1).then(|| config.datasets[0].name.clone()))
            .context(
                "Gateway capture Dataset is ambiguous; use --gateway-dataset or set default_dataset",
            )?,
    };
    config
        .datasets
        .iter()
        .find(|dataset| dataset.name == name)
        .cloned()
        .with_context(|| format!("Gateway capture Dataset '{name}' is not mounted"))
}

fn local_dataset_path(uri: &str) -> Result<Option<PathBuf>> {
    if !uri.contains("://") {
        return Ok(Some(PathBuf::from(uri)));
    }
    let url = Url::parse(uri).context("parse Gateway capture Dataset URI")?;
    match url.scheme() {
        "file" => url
            .to_file_path()
            .map(Some)
            .map_err(|_| anyhow!("convert file Dataset URI to a local path")),
        "local" => Ok(Some(PathBuf::from(url.path()))),
        _ => Ok(None),
    }
}

fn parse_gateway_listener(value: &str, label: &str) -> Result<SocketAddr> {
    let addr = value
        .parse::<SocketAddr>()
        .with_context(|| format!("parse {label} address '{value}'"))?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "pChronicle embedded {label} may only bind to a loopback address"
    );
    Ok(addr)
}

async fn prepare_gateway(
    args: &ServeArgs,
    warehouse: &server::ChronicleServerConfig,
) -> Result<Option<PreparedGateway>> {
    let Some(config_path) = args.gateway.as_deref() else {
        return Ok(None);
    };
    let mut config = persisting_gateway::config::ProxyConfig::from_file(config_path)
        .with_context(|| format!("load Gateway config {}", config_path.display()))?;
    if args.debug {
        config.debug = true;
        persisting_gateway::runtime::debug::enable_debug_stderr();
    }
    let dataset = select_gateway_dataset(warehouse, args.gateway_dataset.as_deref())?;
    let local_dataset = local_dataset_path(&dataset.uri)?;
    let state_dir = match args.gateway_state.clone() {
        Some(path) => path,
        None => local_dataset.clone().with_context(|| {
            format!(
                "Gateway capture Dataset '{}' uses object storage; provide --gateway-state DIRECTORY",
                dataset.name
            )
        })?,
    };
    if args.gateway_object_store_manifest_mode == GatewayObjectStoreManifestMode::SingleWriter {
        anyhow::ensure!(
            local_dataset.is_none(),
            "--gateway-object-store-manifest-mode single-writer requires an object-store Dataset"
        );
    }
    let listen = parse_gateway_listener(&config.listen, "Gateway")?;
    let admin_listen = parse_gateway_listener(&config.admin_listen, "Gateway admin")?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind pChronicle Gateway to {listen}"))?;
    let admin_listener = tokio::net::TcpListener::bind(admin_listen)
        .await
        .with_context(|| format!("bind pChronicle Gateway admin API to {admin_listen}"))?;
    config.listen = listener
        .local_addr()
        .context("read pChronicle Gateway listen address")?
        .to_string();
    config.admin_listen = admin_listener
        .local_addr()
        .context("read pChronicle Gateway admin listen address")?
        .to_string();
    let (sink, writer) = gateway_capture::gateway_capture_sink_with_manifest_write_mode(
        &dataset.uri,
        &config.agent_id,
        args.gateway_object_store_manifest_mode.into(),
    )?;
    Ok(Some(PreparedGateway {
        config,
        state_dir,
        dataset_name: dataset.name,
        stream_markdown: args.gateway_stream_markdown,
        listener,
        admin_listener,
        sink,
        writer,
    }))
}

async fn wait_for_stop(mut receiver: tokio::sync::watch::Receiver<bool>) {
    let _ = receiver.wait_for(|stop| *stop).await;
}

async fn wait_for_termination() {
    #[cfg(unix)]
    {
        let ctrl_c = tokio::signal::ctrl_c();
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = ctrl_c => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = ctrl_c.await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
async fn serve_warehouse_and_gateway(
    warehouse_config: server::ChronicleServerConfig,
    warehouse_listener: tokio::net::TcpListener,
    gateway: PreparedGateway,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    let (diagnostic_tx, diagnostic_rx) = tokio::sync::mpsc::channel(256);
    let mut projections = projection_supervisor::ProjectionSupervisor::new(
        warehouse_config.clone(),
        None,
        diagnostic_tx,
    );
    projections.converge_before_readiness().await?;
    let warehouse = server::PreparedWarehouse::prepare(warehouse_config).await?;
    projections.set_warehouse(Some(warehouse.clone()));
    let mut stderr = Vec::new();
    serve_components(
        Some((warehouse, warehouse_listener)),
        None,
        Some(gateway),
        projections,
        diagnostic_rx,
        &mut stderr,
        shutdown,
    )
    .await
}

async fn serve_gateway_component(
    gateway: PreparedGateway,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let PreparedGateway {
        config,
        state_dir,
        listener,
        admin_listener,
        sink,
        writer,
        stream_markdown,
        ..
    } = gateway;
    let mut gateway_server = Box::pin(persisting_gateway::serve_with_listeners_and_shutdown(
        config,
        state_dir,
        sink,
        stream_markdown,
        listener,
        admin_listener,
        shutdown,
    ));
    let result = (&mut gateway_server).await;
    drop(gateway_server);
    writer
        .finish()
        .context("finish pChronicle Gateway capture")?;
    result
}

async fn serve_components<W: Write + ?Sized>(
    warehouse: Option<(server::PreparedWarehouse, tokio::net::TcpListener)>,
    control: Option<control::PreparedControl>,
    gateway: Option<PreparedGateway>,
    projections: projection_supervisor::ProjectionSupervisor,
    mut diagnostics: tokio::sync::mpsc::Receiver<projection_supervisor::ProjectionDiagnostic>,
    stderr: &mut W,
    shutdown: impl std::future::Future<Output = ()> + Send,
) -> Result<()> {
    type ServiceFuture =
        std::pin::Pin<Box<dyn std::future::Future<Output = (&'static str, Result<()>)> + Send>>;

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut services = FuturesUnordered::<ServiceFuture>::new();
    anyhow::ensure!(
        warehouse.is_some() || control.is_some() || gateway.is_some(),
        "pChronicle serve has no enabled service"
    );
    if let Some((warehouse, listener)) = warehouse {
        let stop = stop_rx.clone();
        services.push(Box::pin(async move {
            (
                "Warehouse",
                server::serve_prepared_warehouse_with_listener_and_shutdown(
                    warehouse,
                    listener,
                    wait_for_stop(stop),
                )
                .await,
            )
        }));
    }
    if let Some(control) = control {
        let stop = stop_rx.clone();
        services.push(Box::pin(async move {
            ("Control", control.serve(wait_for_stop(stop)).await)
        }));
    }
    if let Some(gateway) = gateway {
        let stop = stop_rx.clone();
        services.push(Box::pin(async move {
            (
                "Gateway",
                serve_gateway_component(gateway, wait_for_stop(stop)).await,
            )
        }));
    }
    services.push(Box::pin(async move {
        ("Projection", projections.run(stop_rx).await)
    }));

    tokio::pin!(shutdown);
    let mut diagnostics_open = true;
    let mut diagnostic_error = None;
    let first = loop {
        tokio::select! {
            _ = &mut shutdown => break None,
            completed = services.next() => break completed,
            diagnostic = diagnostics.recv(), if diagnostics_open => {
                match diagnostic {
                    Some(diagnostic) => {
                        if let Err(error) = write_projection_diagnostic(stderr, &diagnostic) {
                            diagnostic_error = Some(error);
                            break None;
                        }
                    }
                    None => diagnostics_open = false,
                }
            }
        }
    };
    let _ = stop_tx.send(true);

    let mut sibling_error = None;
    while !services.is_empty() {
        tokio::select! {
            completed = services.next() => {
                if let Some((name, Err(error))) = completed {
                    sibling_error.get_or_insert_with(|| {
                        error.context(format!("stop pChronicle {name}"))
                    });
                }
            }
            diagnostic = diagnostics.recv(), if diagnostics_open => {
                match diagnostic {
                    Some(diagnostic) => {
                        if diagnostic_error.is_none() {
                            if let Err(error) = write_projection_diagnostic(stderr, &diagnostic) {
                                diagnostic_error = Some(error);
                            }
                        }
                    }
                    None => diagnostics_open = false,
                }
            }
        }
    }
    while let Ok(diagnostic) = diagnostics.try_recv() {
        if diagnostic_error.is_none() {
            if let Err(error) = write_projection_diagnostic(stderr, &diagnostic) {
                diagnostic_error = Some(error);
            }
        }
    }

    if let Some(error) = diagnostic_error {
        return Err(error);
    }

    match first {
        Some((name, Err(error))) => Err(error.context(format!("pChronicle {name} stopped"))),
        Some((name, Ok(()))) => bail!("pChronicle {name} stopped unexpectedly"),
        None => match sibling_error {
            Some(error) => Err(error),
            None => Ok(()),
        },
    }
}

fn write_projection_diagnostic<W: Write + ?Sized>(
    stderr: &mut W,
    diagnostic: &projection_supervisor::ProjectionDiagnostic,
) -> Result<()> {
    writeln!(
        stderr,
        "projection source={} output={} status={} retry_ms={}",
        projection_supervisor::sanitize_log_field(&diagnostic.source_path),
        projection_supervisor::sanitize_log_field(&diagnostic.projection_path),
        diagnostic.status,
        diagnostic.retry_ms,
    )
    .context("write pChronicle projection diagnostic")
}

async fn run_serve(args: ServeArgs, stdout: &mut dyn Write, stderr: &mut dyn Write) -> Result<()> {
    let config = resolve_serve_config(&args)?;
    let control_uri = args
        .control
        .is_some()
        .then(|| control_storage_uri(&config).map(str::to_owned))
        .transpose()?;
    if let Some(uri) = control_uri.as_deref() {
        prepare_local_control_storage(uri).await?;
    }
    let (diagnostic_tx, diagnostic_rx) = tokio::sync::mpsc::channel(256);
    let mut projections =
        projection_supervisor::ProjectionSupervisor::new(config.clone(), None, diagnostic_tx);
    projections.converge_before_readiness().await?;
    let warehouse = match args.listen {
        Some(listen) => {
            anyhow::ensure!(
                listen.ip().is_loopback(),
                "pChronicle Warehouse may only bind to a loopback address"
            );
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .with_context(|| format!("bind pChronicle Warehouse to {listen}"))?;
            let warehouse = server::PreparedWarehouse::prepare(config.clone()).await?;
            Some((warehouse, listener))
        }
        None => None,
    };
    projections.set_warehouse(
        warehouse
            .as_ref()
            .map(|(warehouse, _listener)| warehouse.clone()),
    );
    let control = match args.control {
        Some(listen) => Some(
            control::PreparedControl::bind(
                control_uri.as_deref().context(
                    "pChronicle Control requires a Dataset named 'default'; pass --storage default=URI",
                )?,
                listen,
            )
            .await?,
        ),
        None => None,
    };
    let gateway = prepare_gateway(&args, &config).await?;

    let warehouse_endpoint = warehouse
        .as_ref()
        .map(|(_, listener)| listener.local_addr().map(|addr| addr.to_string()))
        .transpose()
        .context("read pChronicle Warehouse listen address")?;
    let control_ready = control.as_ref().map(control::PreparedControl::ready);
    let gateway_endpoint = gateway
        .as_ref()
        .map(|gateway| gateway.config.listen.clone());
    let gateway_admin_endpoint = gateway
        .as_ref()
        .map(|gateway| gateway.config.admin_listen.clone());
    let ready = ChronicleServeReady {
        version: CHRONICLE_SERVE_READY_VERSION,
        warehouse_endpoint: warehouse_endpoint.clone(),
        control: control_ready,
        gateway_endpoint,
        gateway_admin_endpoint,
    };
    serde_json::to_writer(&mut *stdout, &ready).context("encode pChronicle serve readiness")?;
    writeln!(stdout).context("write pChronicle serve readiness")?;
    stdout.flush().context("flush pChronicle serve readiness")?;

    if let Some(endpoint) = &warehouse_endpoint {
        writeln!(stderr, "pChronicle Warehouse: http://{endpoint}/")
            .context("write pChronicle Warehouse address")?;
    }
    if let Some(ready) = &ready.control {
        writeln!(stderr, "pChronicle Control: {}", ready.endpoint)
            .context("write pChronicle Control address")?;
    }
    if let Some(gateway) = &gateway {
        writeln!(
            stderr,
            "pChronicle Gateway: http://{}/ dataset={}",
            gateway.config.listen, gateway.dataset_name
        )
        .context("write pChronicle Gateway address")?;
        writeln!(
            stderr,
            "pChronicle Gateway admin: http://{}/",
            gateway.config.admin_listen
        )
        .context("write pChronicle Gateway admin address")?;
        if args.debug {
            writeln!(
                stderr,
                "pChronicle Gateway debug: stderr (request/response bodies may be included)"
            )
            .context("write pChronicle Gateway debug status")?;
        }
    }
    if args.open {
        let endpoint = warehouse_endpoint
            .as_deref()
            .context("--open requires --listen")?;
        open_browser(&format!("http://{endpoint}/"))?;
    }
    serve_components(
        warehouse,
        control,
        gateway,
        projections,
        diagnostic_rx,
        stderr,
        wait_for_termination(),
    )
    .await
}

async fn prepare_local_control_storage(uri: &str) -> Result<()> {
    let Some(path) = local_dataset_path(uri)? else {
        return Ok(());
    };
    tokio::fs::create_dir_all(&path)
        .await
        .with_context(|| format!("create pChronicle Control storage root {}", path.display()))
}

fn resolve_serve_config(args: &ServeArgs) -> Result<server::ChronicleServerConfig> {
    match (args.config.as_deref(), args.storage.as_slice()) {
        (Some(config), []) => load_warehouse_config(config),
        (None, storage) if !storage.is_empty() => {
            let mut config =
                server::ChronicleServerConfig::mounted(resolve_storage_mounts(storage)?)?;
            if config
                .datasets
                .iter()
                .any(|dataset| dataset.name == SERVE_STORAGE_DATASET_NAME)
            {
                config.default_dataset = Some(SERVE_STORAGE_DATASET_NAME.into());
            }
            // A single unreadable source (for example a trajectory file that
            // exceeds max_file_bytes) must degrade to an error source instead
            // of preventing the Warehouse from serving the remaining data.
            config.catalog_options.error_policy = CatalogErrorPolicy::Report;
            Ok(config)
        }
        _ => bail!("serve requires exactly one of --config or --storage"),
    }
}

fn resolve_storage_mounts(storages: &[String]) -> Result<Vec<DatasetMount>> {
    anyhow::ensure!(!storages.is_empty(), "serve requires --storage");
    let parsed = storages
        .iter()
        .map(|value| parse_storage_argument(value))
        .collect::<Result<Vec<_>>>()?;
    if parsed.len() == 1 {
        let (name, uri) = &parsed[0];
        let name = name.as_deref().unwrap_or(SERVE_STORAGE_DATASET_NAME);
        return Ok(vec![DatasetMount::new(name, uri.clone())?]);
    }
    parsed
        .into_iter()
        .map(|(name, uri)| {
            let name = match name {
                Some(name) => name,
                None => derived_dataset_name(&uri)?,
            };
            DatasetMount::new(name, uri)
        })
        .collect()
}

fn parse_storage_argument(raw: &str) -> Result<(Option<String>, String)> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "--storage URI must not be empty");
    if let Some((name, uri)) = raw.split_once('=') {
        if looks_like_dataset_name(name) {
            let uri = uri.trim();
            anyhow::ensure!(!uri.is_empty(), "--storage NAME=URI must include a URI");
            return Ok((
                Some(DatasetMount::new(name, "validation")?.name),
                uri.to_string(),
            ));
        }
    }
    Ok((None, raw.to_string()))
}

fn looks_like_dataset_name(name: &str) -> bool {
    DatasetMount::new(name, "validation").is_ok()
}

fn derived_dataset_name(uri: &str) -> Result<String> {
    let basename = storage_basename(uri)?;
    sanitize_derived_dataset_name(&basename).with_context(|| {
        format!("cannot derive Dataset name from '{uri}'; pass --storage NAME=URI")
    })
}

fn storage_basename(uri: &str) -> Result<String> {
    if uri.contains("://") {
        let url = Url::parse(uri).with_context(|| format!("parse --storage URI '{uri}'"))?;
        if let Some(segment) = url
            .path_segments()
            .into_iter()
            .flatten()
            .rev()
            .find(|segment| !segment.is_empty())
        {
            return Ok(segment.to_string());
        }
        if let Some(host) = url.host_str() {
            return Ok(host.to_string());
        }
        bail!("cannot derive Dataset name from '{uri}'; pass --storage NAME=URI");
    }
    match Path::new(uri).file_name().and_then(|name| name.to_str()) {
        Some(name) if name != "." && name != ".." => Ok(name.to_string()),
        _ => bail!("cannot derive Dataset name from '{uri}'; pass --storage NAME=URI"),
    }
}

fn sanitize_derived_dataset_name(raw: &str) -> Result<String> {
    let name: String = raw
        .chars()
        .map(|character| match character {
            '-' | '.' => '_',
            other => other,
        })
        .collect();
    DatasetMount::new(&name, "validation")
        .map(|mount| mount.name)
        .with_context(|| {
            format!(
                "derived Dataset name from '{raw}' is not a valid SQL alias; pass --storage NAME=URI"
            )
        })
}

fn control_storage_uri(config: &server::ChronicleServerConfig) -> Result<&str> {
    config
        .datasets
        .iter()
        .find(|dataset| dataset.name == SERVE_STORAGE_DATASET_NAME)
        .map(|dataset| dataset.uri.as_str())
        .context(
            "pChronicle Control requires a Dataset named 'default'; pass --storage default=URI",
        )
}

async fn run_echo(args: EchoArgs, stderr: &mut dyn Write) -> Result<()> {
    anyhow::ensure!(
        args.listen.ip().is_loopback(),
        "pChronicle Echo may only bind to a loopback address"
    );
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind pChronicle Echo to {}", args.listen))?;
    let addr = listener
        .local_addr()
        .context("read pChronicle Echo listen address")?;
    let default_encoding = match args.encoding {
        EchoEncoding::Plain => persisting_gateway::echo::EchoEncoding::Plain,
        EchoEncoding::Base64 => persisting_gateway::echo::EchoEncoding::Base64,
    };
    writeln!(
        stderr,
        "pChronicle Echo: http://{addr}/ encoding={default_encoding}"
    )
    .context("write pChronicle Echo address")?;
    persisting_gateway::echo::serve_with_shutdown(
        listener,
        persisting_gateway::echo::EchoServerConfig { default_encoding },
        wait_for_termination(),
    )
    .await
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
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let dataset_uri = resolve_dataset_uri(args.dataset_uri.as_deref(), settings_override)?;
    let (dataset_uri, snapshot) =
        discover_snapshot(&dataset_uri, args.errors, args.max_files, args.max_entries).await?;
    let dataset = snapshot
        .dataset(DEFAULT_DATASET_NAME)
        .context("default Dataset missing from Catalog Snapshot")?;
    let response = ListResponse {
        dataset_uri,
        snapshot_id: snapshot.snapshot_id().to_string(),
        created_at: snapshot.created_at().to_string(),
        sources: dataset.sources.iter().map(source_response).collect(),
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

fn source_response(source: &DiscoveredSource) -> SourceResponse {
    SourceResponse {
        source_path: source.file.clone(),
        format: source.format.clone(),
        kind: source.kind,
        snapshot_ref: source.snapshot_ref(),
        size_bytes: source.size_bytes,
        last_modified: source.last_modified.clone(),
        status: source.status,
        error: (source.status == CatalogSourceStatus::Error)
            .then(|| "Source discovery failed".into()),
    }
}

async fn run_status(
    args: StatusArgs,
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(
        args.timeout_seconds > 0,
        "--timeout-seconds must be greater than zero"
    );
    let dataset_uri = resolve_dataset_uri(args.dataset_uri.as_deref(), settings_override)?;
    let (dataset_uri, snapshot) =
        discover_snapshot(&dataset_uri, args.errors, args.max_files, args.max_entries).await?;
    let snapshot = Arc::new(snapshot);
    let inventory = automatic_projection_inventory(snapshot.as_ref())?;
    let mut projections = stream::iter(inventory.targets)
        .map(|target| async move {
            let source_path = target.source_path.clone();
            let projection_path = target.projection_path.clone();
            match inspect_automatic_storyline_projection(&target).await {
                Ok(inspection) => ProjectionStatusResponse::from_inspection(
                    source_path,
                    projection_path,
                    inspection,
                ),
                Err(error) => {
                    tracing::error!(
                        error = ?error,
                        source = %source_path,
                        "pChronicle projection status inspection failed"
                    );
                    ProjectionStatusResponse::error(source_path, projection_path)
                }
            }
        })
        .buffered(STATUS_PROJECTION_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    projections.extend(
        inventory
            .errors
            .into_iter()
            .map(|error| ProjectionStatusResponse::error(error.source_path, error.projection_path)),
    );
    projections.sort_by(|left, right| left.source_path.cmp(&right.source_path));
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
            error: "Source discovery failed".into(),
        })
        .collect::<Vec<_>>();
    let engine = snapshot.clone().query_engine(Default::default()).await?;
    let timeout = Duration::from_secs(args.timeout_seconds);
    let deadline = tokio::time::Instant::now() + timeout;
    let counts = match query_status_counts(&engine, None, deadline, timeout).await {
        Ok(counts) if source_errors.is_empty() => counts,
        Ok(_) if args.errors == ErrorMode::Report => {
            query_reported_status_counts(
                &engine,
                &dataset.sources,
                &mut source_errors,
                deadline,
                timeout,
            )
            .await
        }
        Err(_) if args.errors == ErrorMode::Report => {
            query_reported_status_counts(
                &engine,
                &dataset.sources,
                &mut source_errors,
                deadline,
                timeout,
            )
            .await
        }
        Err(error) => return Err(error),
        Ok(counts) => counts,
    };
    source_errors.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    source_errors.dedup_by(|left, right| left.source_path == right.source_path);
    let error_sources = source_errors.len();
    let ready_sources = total_sources.saturating_sub(error_sources);
    let response = StatusResponse {
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
        projections,
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

fn reported_status_source_failure(source_path: String, error: anyhow::Error) -> StatusSourceError {
    tracing::error!(
        error = ?error,
        source = %source_path,
        "pChronicle Dataset source status query failed"
    );
    StatusSourceError {
        source_path,
        error: "Source status query failed".into(),
    }
}

async fn query_reported_status_counts(
    engine: &ChronicleQueryEngine,
    sources: &[DiscoveredSource],
    source_errors: &mut Vec<StatusSourceError>,
    deadline: tokio::time::Instant,
    timeout: Duration,
) -> StatusCounts {
    let mut counts = StatusCounts::default();
    for source in sources
        .iter()
        .filter(|source| source.status == CatalogSourceStatus::Ready)
    {
        if source_errors
            .iter()
            .any(|error| error.source_path == source.file)
        {
            continue;
        }
        match query_status_counts(engine, Some(&source.file), deadline, timeout).await {
            Ok(source_counts) => counts += source_counts,
            Err(error) => {
                source_errors.push(reported_status_source_failure(source.file.clone(), error))
            }
        }
    }
    counts
}

async fn run_query(
    args: QueryArgs,
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let (dataset_uri, sql) = query_inputs(&args, settings_override)?;
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
    let (dataset_label, _, snapshot) = discover_query_snapshot(
        dataset_uri.as_deref(),
        &args.datasets,
        args.max_files,
        args.max_entries,
    )
    .await?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let engine = snapshot.query_engine(Default::default()).await?;
    let mut buffer = LimitedBuffer::new(args.max_output_bytes);
    let query_result = tokio::time::timeout(
        Duration::from_secs(args.timeout_seconds),
        engine.write_query_jsonl_bounded(sql, &mut buffer, Some(args.max_output_rows)),
    )
    .await;
    let output = match query_result {
        Ok(result) => buffer.finish(result)?,
        Err(_) => bail!(
            "Dataset query timed out after {} seconds",
            args.timeout_seconds
        ),
    };
    let jsonl = match output {
        QueryOutputBudgetOutcome::Complete(bytes) => {
            String::from_utf8(bytes).context("query JSONL is not UTF-8")?
        }
        QueryOutputBudgetOutcome::RowLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "SQL result exceeds max_output_rows limit of {}",
                    args.max_output_rows
                ),
            ));
        }
        QueryOutputBudgetOutcome::ByteLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "SQL result exceeds max_output_bytes limit of {}",
                    args.max_output_bytes
                ),
            ));
        }
    };
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
    ensure_output_byte_budget(output.len(), args.max_output_bytes, "encoded SQL result")?;
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

async fn run_analysis(
    args: AnalysisArgs,
    settings_override: Option<&Path>,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let (analysis, options, sql) = match args.command {
        AnalysisCommand::Overview(options) => ("overview", options, ANALYSIS_OVERVIEW_SQL),
        AnalysisCommand::Agents(options) => ("agents", options, ANALYSIS_AGENTS_SQL),
        AnalysisCommand::Models(options) => ("models", options, ANALYSIS_MODELS_SQL),
        AnalysisCommand::Tools(options) => ("tools", options, ANALYSIS_TOOLS_SQL),
    };
    anyhow::ensure!(options.limit > 0, "--limit must be greater than zero");
    anyhow::ensure!(
        options.limit <= 10_000,
        "--limit must not exceed 10000; use query for larger custom results"
    );
    anyhow::ensure!(
        options.max_output_bytes > 0,
        "--max-output-bytes must be greater than zero"
    );
    anyhow::ensure!(
        options.timeout_seconds > 0,
        "--timeout-seconds must be greater than zero"
    );
    let dataset = resolve_dataset_uri(options.dataset_uri.as_deref(), settings_override)?;
    let (_, _, snapshot) =
        discover_query_snapshot(Some(&dataset), &[], options.max_files, options.max_entries)
            .await?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let engine = snapshot.query_engine(Default::default()).await?;
    let bounded_sql = format!("{sql}\nLIMIT {}", options.limit);
    let mut buffer = LimitedBuffer::new(options.max_output_bytes);
    let query_result = tokio::time::timeout(
        Duration::from_secs(options.timeout_seconds),
        engine.write_query_jsonl_bounded(&bounded_sql, &mut buffer, Some(options.limit)),
    )
    .await;
    let output = match query_result {
        Ok(result) => buffer.finish(result)?,
        Err(_) => bail!(
            "Dataset analysis timed out after {} seconds",
            options.timeout_seconds
        ),
    };
    let jsonl = match output {
        QueryOutputBudgetOutcome::Complete(bytes) => {
            String::from_utf8(bytes).context("analysis JSONL is not UTF-8")?
        }
        QueryOutputBudgetOutcome::RowLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!("analysis result exceeds row limit of {}", options.limit),
            ));
        }
        QueryOutputBudgetOutcome::ByteLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "analysis result exceeds max_output_bytes limit of {}",
                    options.max_output_bytes
                ),
            ));
        }
    };
    let rows = parse_jsonl_rows(&jsonl)?;
    let format = match options.format {
        QueryOutputFormat::Auto if stdout_is_terminal => QueryOutputFormat::Table,
        QueryOutputFormat::Auto => QueryOutputFormat::Jsonl,
        explicit => explicit,
    };
    let output = match format {
        QueryOutputFormat::Table => encode_query_table(&rows)?,
        QueryOutputFormat::Jsonl => jsonl.into_bytes(),
        QueryOutputFormat::Csv => encode_query_csv(&rows),
        QueryOutputFormat::Auto => unreachable!("auto output format was resolved"),
    };
    ensure_output_byte_budget(
        output.len(),
        options.max_output_bytes,
        "encoded analysis result",
    )?;
    stdout
        .write_all(&output)
        .context("write pChronicle analysis output")?;
    writeln!(
        stderr,
        "snapshot_id={} analysis={} rows={} format={} output_bytes={}",
        snapshot_id,
        analysis,
        rows.values.len(),
        query_format_name(format),
        output.len(),
    )
    .context("write pChronicle analysis metadata")?;
    Ok(())
}

const ANALYSIS_OVERVIEW_SQL: &str = r#"
SELECT
  (SELECT COUNT(*) FROM dataset.sources) AS sources,
  (SELECT COUNT(*) FROM dataset.sources WHERE status = 'ready') AS ready_sources,
  (SELECT COUNT(*) FROM dataset.sources WHERE status = 'error') AS error_sources,
  (SELECT COUNT(*) FROM dataset.runs) AS trajectories,
  (SELECT COUNT(*) FROM dataset.steps) AS steps,
  (SELECT COUNT(*) FROM dataset.steps WHERE source = 'user') AS user_steps,
  (SELECT COUNT(*) FROM dataset.steps WHERE source = 'agent') AS agent_steps,
  (SELECT COUNT(*) FROM dataset.tool_calls) AS tool_calls,
  (SELECT COUNT(DISTINCT agent_id) FROM dataset.runs) AS agents,
  (SELECT COUNT(*) FROM (
     SELECT agent_model_name AS model FROM dataset.runs WHERE agent_model_name IS NOT NULL
     UNION
     SELECT model_name AS model FROM dataset.steps WHERE model_name IS NOT NULL
   ) models) AS models
"#;

const ANALYSIS_AGENTS_SQL: &str = r#"
WITH step_stats AS (
  SELECT _file_, document_id,
         COUNT(*) AS steps,
         SUM(CASE WHEN source = 'user' THEN 1 ELSE 0 END) AS user_steps,
         SUM(CASE WHEN source = 'agent' THEN 1 ELSE 0 END) AS agent_steps
  FROM dataset.steps
  GROUP BY _file_, document_id
), tool_stats AS (
  SELECT _file_, document_id, COUNT(*) AS tool_calls
  FROM dataset.tool_calls
  GROUP BY _file_, document_id
)
SELECT r.agent_id,
       COALESCE(r.agent_name, '') AS agent_name,
       COALESCE(r.agent_version, '') AS agent_version,
       COUNT(*) AS trajectories,
       COUNT(DISTINCT r._file_) AS sources,
       SUM(COALESCE(s.steps, 0)) AS steps,
       SUM(COALESCE(s.user_steps, 0)) AS user_steps,
       SUM(COALESCE(s.agent_steps, 0)) AS agent_steps,
       SUM(COALESCE(t.tool_calls, 0)) AS tool_calls
FROM dataset.runs r
LEFT JOIN step_stats s ON r._file_ = s._file_ AND r.document_id = s.document_id
LEFT JOIN tool_stats t ON r._file_ = t._file_ AND r.document_id = t.document_id
GROUP BY r.agent_id, r.agent_name, r.agent_version
ORDER BY trajectories DESC, r.agent_id ASC
"#;

const ANALYSIS_MODELS_SQL: &str = r#"
SELECT model,
       SUM(declared_trajectories) AS declared_trajectories,
       SUM(observed_steps) AS observed_steps
FROM (
  SELECT agent_model_name AS model, COUNT(*) AS declared_trajectories, 0 AS observed_steps
  FROM dataset.runs
  WHERE agent_model_name IS NOT NULL AND agent_model_name <> ''
  GROUP BY agent_model_name
  UNION ALL
  SELECT model_name AS model, 0 AS declared_trajectories, COUNT(*) AS observed_steps
  FROM dataset.steps
  WHERE model_name IS NOT NULL AND model_name <> ''
  GROUP BY model_name
) usage
GROUP BY model
ORDER BY observed_steps DESC, declared_trajectories DESC, model ASC
"#;

const ANALYSIS_TOOLS_SQL: &str = r#"
WITH per_trajectory AS (
  SELECT _file_, document_id, function_name,
         COUNT(*) AS calls,
         COUNT(duration_ms) AS duration_samples,
         SUM(COALESCE(duration_ms, 0)) AS total_duration_ms
  FROM dataset.tool_calls
  GROUP BY _file_, document_id, function_name
)
SELECT function_name,
       SUM(calls) AS calls,
       COUNT(*) AS trajectories,
       COUNT(DISTINCT _file_) AS sources,
       SUM(duration_samples) AS duration_samples,
       SUM(total_duration_ms) AS total_duration_ms
FROM per_trajectory
GROUP BY function_name
ORDER BY calls DESC, function_name ASC
"#;

async fn run_find(
    args: FindArgs,
    settings_override: Option<&Path>,
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
    if let Some(document_id) = &args.document_id {
        validate_find_id("--document-id", document_id)?;
    }
    if let Some(session_id) = &args.session_id {
        validate_find_id("--session-id", session_id)?;
    }
    let dataset = resolve_dataset_uri(args.dataset_uri.as_deref(), settings_override)?;
    let (_, dataset_uris, snapshot) =
        discover_query_snapshot(Some(&dataset), &[], args.max_files, args.max_entries).await?;
    let dataset_uri = dataset_uris
        .first()
        .cloned()
        .context("find Dataset URI missing after discovery")?;
    let snapshot = Arc::new(snapshot);
    let snapshot_id = snapshot.snapshot_id().to_string();
    let engine = snapshot.query_engine(Default::default()).await?;
    let sql = find_sql(&args)?;
    let mut buffer = LimitedBuffer::new(args.max_output_bytes);
    let max_query_rows = args
        .max_results
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .context("--max-results is too large")?;
    let query_result = tokio::time::timeout(
        Duration::from_secs(args.timeout_seconds),
        engine.write_query_jsonl_bounded(&sql, &mut buffer, Some(max_query_rows)),
    )
    .await;
    let output = match query_result {
        Ok(result) => buffer.finish(result)?,
        Err(_) => bail!(
            "Dataset find timed out after {} seconds",
            args.timeout_seconds
        ),
    };
    let jsonl = match output {
        QueryOutputBudgetOutcome::Complete(bytes) => {
            String::from_utf8(bytes).context("find JSONL is not UTF-8")?
        }
        QueryOutputBudgetOutcome::RowLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!("find result exceeds row limit of {max_query_rows}"),
            ));
        }
        QueryOutputBudgetOutcome::ByteLimitExceeded => {
            return Err(cli_boundary_error(
                BoundaryCode::ResourceExhausted,
                format!(
                    "find result exceeds max_output_bytes limit of {}",
                    args.max_output_bytes
                ),
            ));
        }
    };
    let mut matches = jsonl
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("decode pChronicle find match"))
        .collect::<Result<Vec<FindMatch>>>()?;
    let truncated = matches.len() > args.max_results;
    matches.truncate(args.max_results);
    let response = FindResponse {
        dataset_uri,
        snapshot_id,
        query: FindQueryResponse {
            source: args.source,
            document_id: args.document_id,
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
            ensure_output_byte_budget(output.len(), args.max_output_bytes, "encoded find result")?;
            stdout
                .write_all(&output)
                .context("write pChronicle find table")?;
        }
        OutputFormat::Json => {
            let mut output =
                serde_json::to_vec_pretty(&response).context("encode pChronicle find JSON")?;
            output.push(b'\n');
            ensure_output_byte_budget(output.len(), args.max_output_bytes, "encoded find result")?;
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

fn ensure_output_byte_budget(size: usize, max_bytes: usize, label: &str) -> Result<()> {
    if size > max_bytes {
        return Err(cli_boundary_error(
            BoundaryCode::ResourceExhausted,
            format!("{label} exceeds max_output_bytes limit of {max_bytes}"),
        ));
    }
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
    if let Some(document_id) = &args.document_id {
        predicates.push(format!("document_id = {}", sql_string(document_id)));
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
        "_file_ AS source_path, document_id, run_id, session_id, step_id, \
         source AS step_source, effective_kind, timestamp"
    } else {
        "_file_ AS source_path, document_id, run_id, session_id, \
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
                "DOCUMENT ID",
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
            ["SOURCE", "DOCUMENT ID", "RUN ID", "SESSION ID"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
    }
    for candidate in &response.matches {
        let mut row = vec![
            truncate(&candidate.source_path, 64),
            candidate.document_id.clone(),
            candidate.run_id.as_deref().unwrap_or("-").to_string(),
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

fn query_inputs<'a>(
    args: &'a QueryArgs,
    settings_override: Option<&Path>,
) -> Result<(Option<String>, &'a str)> {
    if args.datasets.is_empty() {
        match (&args.dataset_uri, &args.sql) {
            (Some(sql), None) => Ok((Some(resolve_default_warehouse(settings_override)?), sql)),
            (Some(dataset_uri), Some(sql)) => Ok((Some(dataset_uri.clone()), sql)),
            (None, _) => bail!(
                "query requires SQL, optionally preceded by <DATASET_URI>, or --dataset NAME=URI"
            ),
        }
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
        CatalogSnapshotOptions::default()
            .with_error_policy(CatalogErrorPolicy::Strict)
            .with_discovery_limits(max_files, max_entries),
    )
    .await
    .context("discover query Dataset Sources")?;
    Ok((dataset_label, dataset_uris, snapshot))
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
        CatalogSnapshotOptions::default()
            .with_error_policy(errors.into())
            .with_discovery_limits(max_files, max_entries),
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

#[cfg(test)]
mod tests;
