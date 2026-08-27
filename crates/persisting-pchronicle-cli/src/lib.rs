#![recursion_limit = "256"]

mod agent;
mod control;
mod exchange;
mod gateway_capture;
mod gateway_ingest;
mod gateway_partition;
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

use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::fmt::Write as _;
use std::io::{Error as IoError, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use futures::{StreamExt, stream, stream::FuturesUnordered};
use persisting_events::{CHRONICLE_SERVE_READY_VERSION, ChronicleServeReady};
use persisting_pchronicle::document::{
    DocumentFormat, InputIssue, InputIssueKind, decode_json_storylines, detect_format,
    encode_json_storylines, open_document,
};
use persisting_pchronicle::model::StorylineDocument;
use persisting_pchronicle::query::ChronicleQueryEngine;
use persisting_pchronicle::storage::{
    AutomaticProjectionInspection, AutomaticProjectionState, CatalogErrorPolicy,
    CatalogSnapshotOptions, CatalogSourceKind, CatalogSourceStatus, CatalogStorylineKey,
    DEFAULT_DATASET_NAME, DatasetCatalogSnapshot, DatasetLocation, DatasetMount, DiscoveredSource,
    EventFactSnapshot, ObjectStoreManifestWriteMode, StorylineLanceStore,
    StorylineProjectionBuildOutcome, automatic_projection_inventory, build_storyline_projection,
    inspect_automatic_storyline_projection, probe_canonical_event_store,
};
use serde::{Deserialize, Serialize};

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

pub fn error_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<CliBoundaryError>()
        .map(|error| error.code.as_str())
        .unwrap_or("internal")
}

pub fn error_exit_code(error: &anyhow::Error) -> u8 {
    match error
        .downcast_ref::<CliBoundaryError>()
        .map(|error| error.code)
    {
        Some(BoundaryCode::InvalidRequest | BoundaryCode::Unsupported) => 2,
        Some(BoundaryCode::NotFound) => 3,
        Some(BoundaryCode::Conflict) => 4,
        Some(BoundaryCode::ResourceExhausted) => 5,
        Some(BoundaryCode::Unavailable) => 6,
        Some(BoundaryCode::Internal) | None => 1,
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "pchronicle",
    version,
    about = "Learn, browse, query, and exchange Agent trajectory Datasets"
)]
pub struct Cli {
    /// Override the pChronicle user configuration file.
    #[arg(
        short = 'c',
        long = "config",
        global = true,
        value_name = "FILE",
        alias = "settings"
    )]
    config: Option<PathBuf>,

    /// Control stderr diagnostics without changing command results.
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    log_level: LogLevel,

    /// Compatibility alias for --log-level debug.
    #[arg(long, global = true, hide = true)]
    debug_errors: bool,

    #[command(subcommand)]
    command: Command,
}

impl Cli {
    pub fn debug_errors(&self) -> bool {
        self.debug_errors || self.log_level == LogLevel::Debug
    }

    pub fn log_level(&self) -> LogLevel {
        self.log_level
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

struct DiagnosticWriter<'a> {
    level: LogLevel,
    inner: &'a mut dyn Write,
    pending: Vec<u8>,
}

impl<'a> DiagnosticWriter<'a> {
    fn new(level: LogLevel, inner: &'a mut dyn Write) -> Self {
        Self {
            level,
            inner,
            pending: Vec::new(),
        }
    }

    fn emit(&mut self, line: &[u8]) -> std::io::Result<()> {
        let visible = match self.level {
            LogLevel::Error => false,
            LogLevel::Warn => {
                let text = String::from_utf8_lossy(line).to_ascii_lowercase();
                text.contains("warning") || text.starts_with("warn")
            }
            LogLevel::Info | LogLevel::Debug => true,
        };
        if visible {
            self.inner.write_all(line)?;
        }
        Ok(())
    }
}

impl Write for DiagnosticWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buffer);
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            self.emit(&line)?;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.emit(&line)?;
        }
        self.inner.flush()
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Learn the core pChronicle workflow with a guided Dataset walkthrough.
    Onboard(onboard::OnboardArgs),
    /// Show, set, or clear the local default Dataset.
    Default(DefaultArgs),
    /// Manage named Dataset aliases, similar to git remote.
    Alias(AliasArgs),
    /// List run data sources discovered under a Dataset URI.
    #[command(visible_alias = "list")]
    Ls(ListArgs),
    /// Show Dataset health and aggregate statistics.
    Status(StatusArgs),
    /// Execute read-only SQL over one or more Datasets.
    Query(QueryArgs),
    /// Run a stable built-in analysis over normalized run tables.
    Analysis(AnalysisArgs),
    /// Start Codex or Claude with a pChronicle Dataset analysis skill.
    Agent(agent::AgentArgs),
    /// Locate a Run or Step by its source-local ID.
    Find(FindArgs),
    /// Create a new Dataset from one or more run data sources.
    Import(ImportArgs),
    /// Export complete Trajectories to an exchange format.
    Export(ExportArgs),
    /// Run a deterministic local LLM upstream for Gateway testing.
    #[command(hide = true)]
    Echo(EchoArgs),
    /// Unstable developer tools.
    #[command(hide = true)]
    Dev(DevArgs),
    /// Run explicitly enabled Warehouse, Control, and Gateway services.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Dataset path, URI, or alias. Uses the default Dataset when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Include storage size, modification time, and version columns.
    #[arg(long)]
    physical: bool,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Continue when an individual Source cannot be frozen.
    #[arg(long, value_enum, default_value_t = ErrorMode::Report)]
    errors: ErrorMode,

    /// Maximum number of run data sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct DefaultArgs {
    #[command(subcommand)]
    command: Option<DefaultCommand>,

    /// Compatibility form for `default set DIRECTORY`.
    #[arg(value_name = "DIRECTORY", hide = true)]
    legacy_directory: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum DefaultCommand {
    /// Show the configured local default Dataset.
    Show,
    /// Set the local default Dataset, creating the directory when needed.
    Set {
        #[arg(value_name = "LOCAL_DATASET")]
        dataset: String,
    },
    /// Clear the default without deleting Dataset data.
    Clear,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct AliasArgs {
    #[command(subcommand)]
    command: Option<AliasCommand>,
}

#[derive(Debug, Subcommand)]
enum AliasCommand {
    /// List configured aliases.
    List {
        #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
        format: OutputFormat,
    },
    /// Add a new alias.
    Add {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(value_name = "DATASET")]
        dataset: String,
    },
    /// Print an alias target.
    GetUrl {
        #[arg(value_name = "NAME")]
        name: String,
    },
    /// Change an existing alias target.
    SetUrl {
        #[arg(value_name = "NAME")]
        name: String,
        #[arg(value_name = "DATASET")]
        dataset: String,
    },
    /// Rename an existing alias.
    Rename {
        #[arg(value_name = "OLD")]
        old: String,
        #[arg(value_name = "NEW")]
        new: String,
    },
    /// Remove an alias without deleting its Dataset.
    Remove {
        #[arg(value_name = "NAME")]
        name: String,
    },
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Dataset path, URI, or alias. Uses the default Dataset when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSON when piped.
    #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
    format: OutputFormat,

    /// Fail on a bad Source, or report partial counts and continue.
    #[arg(long, value_enum, default_value_t = ErrorMode::Report)]
    errors: ErrorMode,

    /// Maximum number of run data sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,

    /// Maximum time for run count queries.
    #[arg(long = "timeout", alias = "timeout-seconds", value_name = "DURATION", value_parser = parse_duration_seconds, default_value = "30s")]
    timeout_seconds: u64,
}

#[derive(Debug, Args)]
struct QueryArgs {
    /// Dataset to query. Uses the default Dataset when omitted.
    #[arg(value_name = "DATASET")]
    dataset_uri: Option<String>,

    /// Mount a named Dataset as NAME=DATASET. Repeat for cross-Dataset SQL.
    #[arg(long = "mount", alias = "dataset", value_name = "NAME=DATASET")]
    datasets: Vec<String>,

    /// Compatibility positional SQL statement.
    #[arg(value_name = "SQL", hide = true)]
    sql: Option<String>,

    /// One read-only SQL statement.
    #[arg(long = "sql", value_name = "SQL", conflicts_with_all = ["sql", "file"])]
    sql_option: Option<String>,

    /// Read one SQL statement from FILE, or from stdin with -.
    #[arg(long, value_name = "FILE_OR_STDIN", conflicts_with_all = ["sql", "sql_option"])]
    file: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSONL when piped.
    #[arg(long, value_enum, default_value_t = QueryOutputFormat::Auto)]
    format: QueryOutputFormat,

    /// Write results to a new file instead of stdout. Use - for stdout.
    #[arg(short, long, value_name = "PATH_OR_STDOUT", default_value = "-")]
    output: String,

    /// Replace an existing query output file atomically.
    #[arg(long)]
    overwrite: bool,

    /// Reject results containing more rows than this limit.
    #[arg(long, default_value_t = 100_000)]
    max_output_rows: u64,

    /// Reject intermediate or final encoded results larger than this many bytes.
    #[arg(long, value_parser = parse_byte_size, default_value = "64MiB")]
    max_output_bytes: usize,

    /// Maximum time for SQL execution and result encoding.
    #[arg(long = "timeout", alias = "timeout-seconds", value_name = "DURATION", value_parser = parse_duration_seconds, default_value = "30s")]
    timeout_seconds: u64,

    /// Maximum number of run data sources to discover.
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
    /// Aggregate run activity by Agent identity and version.
    Agents(AnalysisOptions),
    /// Aggregate declared and observed Model usage.
    Models(AnalysisOptions),
    /// Aggregate tool usage and duration coverage.
    #[command(visible_aliases = ["tool-calls", "toolcalls"])]
    Tools(AnalysisOptions),
}

#[derive(Debug, Args)]
struct AnalysisOptions {
    /// Dataset path, URI, or alias. Uses the default Dataset when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Output format. Auto uses a table on a terminal and JSONL when piped.
    #[arg(long, value_enum, default_value_t = QueryOutputFormat::Auto)]
    format: QueryOutputFormat,

    /// Maximum number of grouped rows returned.
    #[arg(long, default_value_t = 100)]
    limit: u64,

    /// Reject encoded results larger than this many bytes.
    #[arg(long, value_parser = parse_byte_size, default_value = "8MiB")]
    max_output_bytes: usize,

    /// Maximum time for analysis execution and result encoding.
    #[arg(long = "timeout", alias = "timeout-seconds", value_name = "DURATION", value_parser = parse_duration_seconds, default_value = "30s")]
    timeout_seconds: u64,

    /// Maximum number of run data sources to discover.
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
    /// Dataset path, URI, or alias. Uses the default Dataset when omitted.
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: Option<String>,

    /// Narrow the lookup to one Dataset-relative Source path.
    #[arg(long)]
    source: Option<String>,

    /// Find candidates by source-local Run ID.
    #[arg(long)]
    run_id: Option<String>,

    /// Find one run record by its stable source-local document ID.
    #[arg(long)]
    document_id: Option<String>,

    /// Find Run or Step candidates by source-local Session ID.
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
    #[arg(long, value_parser = parse_byte_size, default_value = "8MiB")]
    max_output_bytes: usize,

    /// Maximum time for the lookup query.
    #[arg(long = "timeout", alias = "timeout-seconds", value_name = "DURATION", value_parser = parse_duration_seconds, default_value = "30s")]
    timeout_seconds: u64,

    /// Maximum number of run data sources to discover.
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
    Codex,
    #[value(name = "claude-code")]
    ClaudeCode,
}

impl ExchangeFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Atif => "atif",
            Self::Actf => "actf",
            Self::OpenaiMessages => "openai-messages",
            Self::Storyline => "storyline",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }
}

impl std::fmt::Display for ExchangeFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExportFormat {
    Atif,
    Actf,
    #[value(name = "openai-messages")]
    OpenaiMessages,
    Storyline,
}

impl From<ExportFormat> for ExchangeFormat {
    fn from(format: ExportFormat) -> Self {
        match format {
            ExportFormat::Atif => Self::Atif,
            ExportFormat::Actf => Self::Actf,
            ExportFormat::OpenaiMessages => Self::OpenaiMessages,
            ExportFormat::Storyline => Self::Storyline,
        }
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
    /// Input run data file or directory, or - for stdin.
    #[arg(short = 'f', long = "from", value_name = "PATH_OR_STDIN")]
    from: String,

    /// New Dataset directory or object-store URI.
    #[arg(short = 't', long = "to", alias = "output", value_name = "NEW_DATASET")]
    output: Option<String>,

    /// Input exchange format. Auto detects each regular file from name and content.
    /// Directory imports skip JSON that is not a supported run data format.
    #[arg(short = 'i', long = "input-format", alias = "format", value_enum, default_value_t = ExchangeFormat::Auto)]
    format: ExchangeFormat,

    /// Dataset layout: preserve source files, or combine them into one Storyline Lance Store at the Dataset root.
    #[arg(short = 'o', long = "output-format", value_enum)]
    output_format: Option<ImportOutputFormat>,

    /// Compatibility no-op; stdin is selected by --from -.
    #[arg(long, hide = true)]
    stream: bool,

    /// Maximum bytes accepted from each Source, or from stdin in total.
    #[arg(long, value_parser = parse_byte_size, default_value = "256MiB")]
    max_input_bytes: Option<usize>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Dataset to export. This argument is always explicit.
    #[arg(short = 'f', long = "from", value_name = "DATASET", required = true)]
    from: Option<String>,

    /// New local file, object-store URI, or - for stdout.
    #[arg(
        short = 't',
        long = "to",
        alias = "output",
        value_name = "PATH_URI_OR_STDOUT"
    )]
    output: String,

    /// Output exchange format.
    #[arg(short = 'o', long = "output-format", alias = "format", value_enum)]
    format: ExportFormat,

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

    /// Replace an existing output file or object.
    #[arg(long)]
    overwrite: bool,

    /// Write the finite export document to stdout; requires --output -.
    #[arg(long, hide = true)]
    stream: bool,

    /// Maximum number of complete Trajectories to export.
    #[arg(long, default_value_t = 10_000)]
    max_trajectories: u64,

    /// Reject encoded output larger than this many bytes.
    #[arg(long, value_parser = parse_byte_size, default_value = "64MiB")]
    max_output_bytes: usize,

    /// Maximum time for address selection and run loading.
    #[arg(long = "timeout", alias = "timeout-seconds", value_name = "DURATION", value_parser = parse_duration_seconds, default_value = "30s")]
    timeout_seconds: u64,

    /// Maximum number of run data sources to discover.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_FILES)]
    max_files: usize,

    /// Maximum number of filesystem entries or objects to inspect.
    #[arg(long, default_value_t = persisting_pchronicle::storage::DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    max_entries: usize,
}

#[derive(Debug, Args)]
#[command(
    override_usage = "pchronicle serve [OPTIONS] <[NAME=]DATASET>...",
    group(
        ArgGroup::new("dataset_source")
            .required(true)
            .multiple(true)
            .args(["config", "storage", "positional_storage", "gateway_dataset"])
    ),
    group(
        ArgGroup::new("storage_source")
            .multiple(true)
            .args(["storage", "positional_storage"])
    ),
    group(
        ArgGroup::new("serve_component")
            .multiple(true)
            .args(["listen", "control", "gateway", "gateway_config"])
    ),
    group(
        ArgGroup::new("gateway_mode")
            .multiple(false)
            .args(["gateway", "gateway_config"])
    )
)]
struct ServeArgs {
    /// Compatibility: static Warehouse configuration file.
    #[arg(
        long = "warehouse-config",
        value_name = "FILE",
        hide = true,
        conflicts_with_all = ["storage", "positional_storage"]
    )]
    config: Option<PathBuf>,

    /// Compatibility: repeatable Dataset mount.
    #[arg(long, value_name = "URI", conflicts_with = "config", hide = true)]
    storage: Vec<String>,

    /// Dataset to serve; NAME=DATASET sets the mount name.
    #[arg(value_name = "[NAME=]DATASET", conflicts_with = "config")]
    positional_storage: Vec<String>,

    /// Loopback address for the read-only API and Web UI.
    /// Defaults to 127.0.0.1:0 when no other service is selected.
    #[arg(long)]
    listen: Option<SocketAddr>,

    /// Loopback address for the authenticated Control protocol.
    #[arg(long, requires = "storage_source", conflicts_with = "config")]
    control: Option<SocketAddr>,

    /// Open the Web UI in the system browser after the listener is ready.
    #[arg(long, requires = "listen")]
    open: bool,

    /// Start the config-free canonical event ingest Gateway.
    /// `auto` selects loopback and an ephemeral port.
    #[arg(
        long,
        value_name = "ADDRESS",
        value_parser = parse_gateway_bind,
        requires = "gateway_dataset"
    )]
    gateway: Option<SocketAddr>,

    /// Compatibility: start the forwarding LLM Gateway from a TOML file.
    #[arg(long, value_name = "FILE", requires = "gateway_dataset")]
    gateway_config: Option<PathBuf>,

    /// Dataset path or object-store URI that receives Gateway events.
    #[arg(long, value_name = "DATASET", requires = "gateway_mode")]
    gateway_dataset: Option<String>,

    /// Relative physical partition template using {user}, {date}, and {hour}.
    #[arg(long, value_name = "TEMPLATE", requires = "gateway_mode")]
    gateway_split: Option<String>,

    /// Wait for this long without new Gateway events before refreshing an
    /// existing Storyline projection. New sources are projected immediately.
    #[arg(
        long = "gateway-split-idle",
        value_name = "DURATION",
        value_parser = parse_duration_seconds,
        default_value = "30m",
        requires = "gateway"
    )]
    gateway_split_idle_seconds: u64,

    /// Local Gateway state directory; required for an object-store Dataset.
    #[arg(long, value_name = "DIRECTORY", requires = "gateway_config")]
    gateway_state: Option<PathBuf>,

    /// Object-store manifest publication contract used by Gateway capture.
    #[arg(
        long,
        value_enum,
        default_value_t,
        requires = "gateway_mode",
        value_name = "MODE",
        hide = true
    )]
    gateway_object_store_manifest_mode: GatewayObjectStoreManifestMode,

    /// Also maintain Gateway's live AgenticMD projection.
    #[arg(long, requires = "gateway_config")]
    gateway_stream_markdown: bool,

    /// Print Gateway diagnostics, including size-limited request/response bodies, to stderr.
    #[arg(long = "gateway-debug", alias = "debug", requires = "gateway_config")]
    debug: bool,
}

fn parse_gateway_bind(value: &str) -> std::result::Result<SocketAddr, String> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(SocketAddr::from(([127, 0, 0, 1], 0)));
    }
    let address = value
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid Gateway address '{value}': {error}"))?;
    if !address.ip().is_loopback() {
        return Err("the embedded Gateway is loopback-only; use 127.0.0.1:PORT or 'auto'".into());
    }
    Ok(address)
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

#[derive(Debug, Args)]
struct DevArgs {
    #[command(subcommand)]
    command: DevCommand,
}

#[derive(Debug, Subcommand)]
enum DevCommand {
    /// Run a deterministic local LLM upstream for Gateway tests.
    Echo(EchoArgs),
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

fn parse_duration_seconds(value: &str) -> std::result::Result<u64, String> {
    let value = value.trim();
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        let milliseconds = number
            .parse::<u64>()
            .map_err(|_| format!("invalid duration '{value}'"))?;
        if milliseconds == 0 {
            return Err("duration must be greater than zero".to_owned());
        }
        return Ok(milliseconds.div_ceil(1000));
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 60 * 60)
    } else {
        // Compatibility for the deprecated --timeout-seconds spelling.
        (value, 1)
    };
    let amount = number
        .parse::<u64>()
        .map_err(|_| format!("invalid duration '{value}'; use ms, s, m, or h"))?;
    amount
        .checked_mul(multiplier)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| "duration must be greater than zero and fit in u64 seconds".to_owned())
}

fn parse_byte_size(value: &str) -> std::result::Result<usize, String> {
    let value = value.trim();
    let suffixes = [
        ("KiB", 1024usize),
        ("MiB", 1024usize * 1024),
        ("GiB", 1024usize * 1024 * 1024),
    ];
    let (number, multiplier) = suffixes
        .iter()
        .find_map(|(suffix, multiplier)| {
            value
                .strip_suffix(suffix)
                .map(|number| (number, *multiplier))
        })
        .unwrap_or((value, 1));
    let amount = number
        .parse::<usize>()
        .map_err(|_| format!("invalid byte size '{value}'; use an integer or KiB, MiB, GiB"))?;
    amount
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "byte size must be greater than zero and fit in usize".to_owned())
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
    let config = cli.config.as_deref();
    let mut diagnostics = DiagnosticWriter::new(cli.log_level, stderr);
    match cli.command {
        Command::Onboard(args) => {
            onboard::run(
                args,
                config,
                stdin_is_terminal,
                stdout_is_terminal,
                stdin,
                stdout,
            )
            .await
        }
        Command::Default(args) => run_default(args, config, stdout, &mut diagnostics),
        Command::Alias(args) => {
            run_alias(args, config, stdout_is_terminal, stdout, &mut diagnostics)
        }
        Command::Ls(args) => {
            run_list(args, config, stdout_is_terminal, stdout, &mut diagnostics).await
        }
        Command::Status(args) => {
            run_status(args, config, stdout_is_terminal, stdout, &mut diagnostics).await
        }
        Command::Query(args) => {
            run_query(
                args,
                config,
                stdout_is_terminal,
                stdin,
                stdout,
                &mut diagnostics,
            )
            .await
        }
        Command::Analysis(args) => {
            run_analysis(args, config, stdout_is_terminal, stdout, &mut diagnostics).await
        }
        Command::Agent(args) => agent::run(
            args,
            config,
            stdin_is_terminal,
            stdout_is_terminal,
            stdin,
            stdout,
            &mut diagnostics,
        ),
        Command::Find(args) => {
            run_find(args, config, stdout_is_terminal, stdout, &mut diagnostics).await
        }
        Command::Import(args) => run_import(args, config, stdin, stdout, &mut diagnostics).await,
        Command::Export(args) => run_export(args, config, stdout, &mut diagnostics).await,
        Command::Echo(args) => run_echo(args, &mut diagnostics).await,
        Command::Dev(DevArgs {
            command: DevCommand::Echo(args),
        }) => run_echo(args, &mut diagnostics).await,
        Command::Serve(args) => run_serve(args, config, stdout, &mut diagnostics).await,
    }
}

struct PreparedProxyGateway {
    config: persisting_gateway::config::ProxyConfig,
    state_dir: PathBuf,
    dataset_uri: String,
    split: Option<String>,
    stream_markdown: bool,
    listener: tokio::net::TcpListener,
    admin_listener: tokio::net::TcpListener,
    sink: Arc<dyn persisting_gateway::sink::CaptureEventSink>,
    writer: gateway_capture::GatewayCaptureWriter,
}

enum PreparedGateway {
    Ingest(gateway_ingest::PreparedIngestGateway),
    Proxy(Box<PreparedProxyGateway>),
}

impl PreparedGateway {
    fn endpoint(&self) -> &str {
        match self {
            Self::Ingest(gateway) => gateway.endpoint(),
            Self::Proxy(gateway) => &gateway.config.listen,
        }
    }

    fn admin_endpoint(&self) -> Option<&str> {
        match self {
            Self::Ingest(_) => None,
            Self::Proxy(gateway) => Some(&gateway.config.admin_listen),
        }
    }

    fn dataset_uri(&self) -> &str {
        match self {
            Self::Ingest(gateway) => gateway.dataset_uri(),
            Self::Proxy(gateway) => &gateway.dataset_uri,
        }
    }

    fn split_source(&self) -> Option<&str> {
        match self {
            Self::Ingest(gateway) => gateway.split_source(),
            Self::Proxy(gateway) => gateway.split.as_deref(),
        }
    }
}

const SERVE_STORAGE_DATASET_NAME: &str = "default";

fn local_dataset_path(uri: &str) -> Result<Option<PathBuf>> {
    Ok(DatasetLocation::parse(uri)?
        .local_path()
        .map(Path::to_path_buf))
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
    dataset_uri: Option<&str>,
) -> Result<Option<PreparedGateway>> {
    if args.gateway.is_none() && args.gateway_config.is_none() {
        return Ok(None);
    }
    let dataset_uri = dataset_uri.context("Gateway requires --gateway-dataset DATASET")?;
    let split = args
        .gateway_split
        .as_deref()
        .map(gateway_partition::GatewaySplitTemplate::parse)
        .transpose()?;
    let local_dataset = local_dataset_path(dataset_uri)?;
    if args.gateway_object_store_manifest_mode == GatewayObjectStoreManifestMode::SingleWriter {
        anyhow::ensure!(
            local_dataset.is_none(),
            "--gateway-object-store-manifest-mode single-writer requires an object-store Dataset"
        );
    }

    if let Some(listen) = args.gateway {
        let gateway = gateway_ingest::PreparedIngestGateway::bind(
            listen,
            dataset_uri.to_string(),
            split,
            args.gateway_object_store_manifest_mode.into(),
        )
        .await?;
        return Ok(Some(PreparedGateway::Ingest(gateway)));
    }

    let config_path = args
        .gateway_config
        .as_deref()
        .context("missing Gateway config path")?;
    let mut config = persisting_gateway::config::ProxyConfig::from_file(config_path)
        .with_context(|| format!("load Gateway config {}", config_path.display()))?;
    if args.debug {
        config.debug = true;
        persisting_gateway::runtime::debug::enable_debug_stderr();
    }
    let state_dir = match args.gateway_state.clone() {
        Some(path) => path,
        None => local_dataset.clone().with_context(|| {
            format!(
                "Gateway capture Dataset '{dataset_uri}' uses object storage; provide --gateway-state DIRECTORY"
            )
        })?,
    };
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
        dataset_uri,
        &config.agent_id,
        split.clone(),
        args.gateway_object_store_manifest_mode.into(),
    )?;
    Ok(Some(PreparedGateway::Proxy(Box::new(
        PreparedProxyGateway {
            config,
            state_dir,
            dataset_uri: dataset_uri.to_string(),
            split: split.map(|template| template.source().to_string()),
            stream_markdown: args.gateway_stream_markdown,
            listener,
            admin_listener,
            sink,
            writer,
        },
    ))))
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
    match gateway {
        PreparedGateway::Ingest(gateway) => gateway.serve(shutdown).await,
        PreparedGateway::Proxy(gateway) => {
            let PreparedProxyGateway {
                config,
                state_dir,
                listener,
                admin_listener,
                sink,
                writer,
                stream_markdown,
                ..
            } = *gateway;
            let mut gateway_server =
                Box::pin(persisting_gateway::serve_with_listeners_and_shutdown(
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
    }
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
                        if diagnostic_error.is_none()
                            && let Err(error) = write_projection_diagnostic(stderr, &diagnostic)
                        {
                            diagnostic_error = Some(error);
                        }
                    }
                    None => diagnostics_open = false,
                }
            }
        }
    }
    while let Ok(diagnostic) = diagnostics.try_recv() {
        if diagnostic_error.is_none()
            && let Err(error) = write_projection_diagnostic(stderr, &diagnostic)
        {
            diagnostic_error = Some(error);
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

fn warehouse_listen(args: &ServeArgs) -> Option<SocketAddr> {
    match args.listen {
        Some(listen) => Some(listen),
        None if args.control.is_none()
            && args.gateway.is_none()
            && args.gateway_config.is_none() =>
        {
            Some(SocketAddr::from(([127, 0, 0, 1], 0)))
        }
        None => None,
    }
}

async fn run_serve(
    args: ServeArgs,
    settings_override: Option<&Path>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let gateway_dataset_uri = resolve_gateway_dataset_uri(&args, settings_override)?;
    if let Some(uri) = gateway_dataset_uri.as_deref() {
        prepare_local_gateway_dataset(uri).await?;
    }
    let config = resolve_serve_config_with_settings(&args, settings_override)?;
    let control_uri = args
        .control
        .is_some()
        .then(|| control_storage_uri(&config).map(str::to_owned))
        .transpose()?;
    if let Some(uri) = control_uri.as_deref() {
        prepare_local_control_storage(uri).await?;
    }
    let (diagnostic_tx, diagnostic_rx) = tokio::sync::mpsc::channel(256);
    let projection_idle = if args.gateway.is_some() {
        Duration::from_secs(args.gateway_split_idle_seconds)
    } else {
        Duration::default()
    };
    let mut projections = projection_supervisor::ProjectionSupervisor::with_projection_idle(
        config.clone(),
        None,
        projection_idle,
        diagnostic_tx,
    );
    projections.converge_before_readiness().await?;
    let warehouse = match warehouse_listen(&args) {
        Some(listen) => {
            anyhow::ensure!(
                listen.ip().is_loopback(),
                "pChronicle Warehouse may only bind to a loopback address"
            );
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .with_context(|| format!("bind pChronicle Warehouse to {listen}"))?;
            let warehouse = if args.gateway.is_some() {
                server::PreparedWarehouse::prepare_live(config.clone()).await?
            } else {
                server::PreparedWarehouse::prepare(config.clone()).await?
            };
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
                    "pChronicle Control requires a Dataset named 'default'; pass default=DATASET",
                )?,
                listen,
            )
            .await?,
        ),
        None => None,
    };
    let gateway = prepare_gateway(&args, gateway_dataset_uri.as_deref()).await?;

    let warehouse_endpoint = warehouse
        .as_ref()
        .map(|(_, listener)| listener.local_addr().map(|addr| addr.to_string()))
        .transpose()
        .context("read pChronicle Warehouse listen address")?;
    let control_ready = control.as_ref().map(control::PreparedControl::ready);
    let gateway_endpoint = gateway
        .as_ref()
        .map(|gateway| gateway.endpoint().to_string());
    let gateway_admin_endpoint = gateway
        .as_ref()
        .and_then(|gateway| gateway.admin_endpoint().map(str::to_string));
    let gateway_dataset = gateway
        .as_ref()
        .map(|gateway| gateway.dataset_uri().to_string());
    let gateway_split = gateway
        .as_ref()
        .and_then(|gateway| gateway.split_source().map(str::to_string));
    let ready = ChronicleServeReady {
        version: CHRONICLE_SERVE_READY_VERSION,
        warehouse_endpoint: warehouse_endpoint.clone(),
        control: control_ready,
        gateway_endpoint,
        gateway_admin_endpoint,
        gateway_dataset,
        gateway_split,
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
            gateway.endpoint(),
            gateway.dataset_uri()
        )
        .context("write pChronicle Gateway address")?;
        if let Some(split) = gateway.split_source() {
            writeln!(stderr, "pChronicle Gateway split: {split}")
                .context("write pChronicle Gateway split")?;
        }
        if let Some(admin) = gateway.admin_endpoint() {
            writeln!(stderr, "pChronicle Gateway admin: http://{admin}/")
                .context("write pChronicle Gateway admin address")?;
        }
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

async fn prepare_local_gateway_dataset(uri: &str) -> Result<()> {
    let Some(path) = local_dataset_path(uri)? else {
        return Ok(());
    };
    tokio::fs::create_dir_all(&path)
        .await
        .with_context(|| format!("create pChronicle Gateway Dataset {}", path.display()))
}

fn resolve_gateway_dataset_uri(
    args: &ServeArgs,
    settings_override: Option<&Path>,
) -> Result<Option<String>> {
    args.gateway_dataset
        .as_deref()
        .map(|uri| expand_dataset_reference(uri, settings_override, false))
        .transpose()
}

fn serve_storage_uris(args: &ServeArgs) -> Vec<String> {
    let mut storage = args.storage.clone();
    storage.extend(args.positional_storage.iter().cloned());
    storage
}

fn resolve_serve_config_with_settings(
    args: &ServeArgs,
    settings_override: Option<&Path>,
) -> Result<server::ChronicleServerConfig> {
    let storage = serve_storage_uris(args);
    let gateway_dataset = resolve_gateway_dataset_uri(args, settings_override)?;
    let mut config = match (args.config.as_deref(), storage.as_slice()) {
        (Some(config), []) => load_warehouse_config_with_user_config(config, settings_override)?,
        (None, storage) if !storage.is_empty() => {
            let mut config = server::ChronicleServerConfig::mounted(resolve_storage_mounts(
                storage,
                settings_override,
            )?)?;
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
            config
        }
        (None, []) if gateway_dataset.is_some() => {
            server::ChronicleServerConfig::mounted(vec![DatasetMount::new(
                SERVE_STORAGE_DATASET_NAME,
                gateway_dataset.as_deref().context("Gateway Dataset")?,
            )?])?
        }
        _ => bail!("serve requires at least one Dataset"),
    };
    if let Some(uri) = gateway_dataset {
        ensure_gateway_mount(&mut config, uri)?;
    }
    Ok(config)
}

fn ensure_gateway_mount(config: &mut server::ChronicleServerConfig, uri: String) -> Result<()> {
    if config.datasets.iter().any(|dataset| dataset.uri == uri) {
        return Ok(());
    }
    let name = if config.datasets.is_empty() {
        SERVE_STORAGE_DATASET_NAME
    } else {
        "gateway"
    };
    anyhow::ensure!(
        !config.datasets.iter().any(|dataset| dataset.name == name),
        "cannot auto-mount Gateway Dataset as '{name}'; that name is already mounted"
    );
    config.datasets.push(DatasetMount::new(name, uri)?);
    if config.default_dataset.is_none() && config.datasets.len() == 1 {
        config.default_dataset = Some(name.to_string());
    }
    config.catalog_options.error_policy = CatalogErrorPolicy::Report;
    Ok(())
}

#[cfg(test)]
fn resolve_serve_config(args: &ServeArgs) -> Result<server::ChronicleServerConfig> {
    resolve_serve_config_with_settings(args, None)
}

fn resolve_storage_mounts(
    storages: &[String],
    settings_override: Option<&Path>,
) -> Result<Vec<DatasetMount>> {
    anyhow::ensure!(!storages.is_empty(), "serve requires at least one Dataset");
    let parsed = storages
        .iter()
        .map(|value| parse_storage_argument(value, settings_override))
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

fn parse_storage_argument(
    raw: &str,
    settings_override: Option<&Path>,
) -> Result<(Option<String>, String)> {
    let raw = raw.trim();
    anyhow::ensure!(!raw.is_empty(), "Dataset must not be empty");
    if let Some((name, uri)) = raw.split_once('=')
        && looks_like_dataset_name(name)
    {
        let uri = uri.trim();
        anyhow::ensure!(!uri.is_empty(), "NAME=DATASET must include a Dataset");
        return Ok((
            Some(DatasetMount::new(name, "validation")?.name),
            expand_dataset_reference(uri, settings_override, false)?,
        ));
    }
    Ok((
        None,
        expand_dataset_reference(raw, settings_override, false)?,
    ))
}

fn looks_like_dataset_name(name: &str) -> bool {
    DatasetMount::new(name, "validation").is_ok()
}

fn derived_dataset_name(uri: &str) -> Result<String> {
    let basename = storage_basename(uri)?;
    sanitize_derived_dataset_name(&basename)
        .with_context(|| format!("cannot derive Dataset name from '{uri}'; pass NAME=DATASET"))
}

fn storage_basename(uri: &str) -> Result<String> {
    let location = DatasetLocation::parse(uri)?;
    if let Some(path) = location.local_path() {
        return match path.file_name().and_then(|name| name.to_str()) {
            Some(name) if name != "." && name != ".." => Ok(name.to_string()),
            _ => bail!("cannot derive Dataset name from '{uri}'; pass NAME=DATASET"),
        };
    }
    let rest = location
        .as_str()
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(location.as_str());
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    if let Some(segment) = path.split('/').rev().find(|segment| !segment.is_empty()) {
        return Ok(segment.to_string());
    }
    if !host.is_empty() {
        return Ok(host.to_string());
    }
    bail!("cannot derive Dataset name from '{uri}'; pass NAME=DATASET")
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
            format!("derived Dataset name from '{raw}' is not a valid SQL alias; pass NAME=DATASET")
        })
}

fn control_storage_uri(config: &server::ChronicleServerConfig) -> Result<&str> {
    config
        .datasets
        .iter()
        .find(|dataset| dataset.name == SERVE_STORAGE_DATASET_NAME)
        .map(|dataset| dataset.uri.as_str())
        .context("pChronicle Control requires a Dataset named 'default'; pass default=DATASET")
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
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    let (dataset_uri, sql) = query_inputs(&args, settings_override, stdin)?;
    let datasets = resolve_query_mounts(&args.datasets, settings_override)?;
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
        &datasets,
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
        engine.write_query_jsonl_bounded(&sql, &mut buffer, Some(args.max_output_rows)),
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
    write_query_output(&args.output, &output, args.overwrite, stdout)?;
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

fn query_inputs(
    args: &QueryArgs,
    settings_override: Option<&Path>,
    stdin: &mut dyn Read,
) -> Result<(Option<String>, String)> {
    let canonical_sql = match (&args.sql_option, &args.file) {
        (Some(sql), None) => Some(sql.clone()),
        (None, Some(file)) => Some(read_sql_input(file, stdin)?),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap rejects --sql with --file"),
    };
    if !args.datasets.is_empty() {
        let sql = match (canonical_sql, &args.dataset_uri, &args.sql) {
            (Some(sql), None, None) => sql,
            (None, Some(legacy_sql), None) => legacy_sql.clone(),
            _ => bail!("query with --mount requires exactly one of --sql or --file"),
        };
        return Ok((None, sql));
    }
    match (canonical_sql, &args.dataset_uri, &args.sql) {
        (Some(sql), dataset, None) => Ok((
            Some(resolve_dataset_uri(dataset.as_deref(), settings_override)?),
            sql,
        )),
        (None, Some(legacy_sql), None) => Ok((
            Some(resolve_default_warehouse(settings_override)?),
            legacy_sql.clone(),
        )),
        (None, Some(dataset), Some(legacy_sql)) => Ok((
            Some(resolve_dataset_uri(Some(dataset), settings_override)?),
            legacy_sql.clone(),
        )),
        _ => bail!("query requires exactly one of --sql or --file"),
    }
}

fn read_sql_input(path: &str, stdin: &mut dyn Read) -> Result<String> {
    const MAX_SQL_BYTES: u64 = 1024 * 1024;
    let mut input = String::new();
    if path == "-" {
        stdin
            .take(MAX_SQL_BYTES + 1)
            .read_to_string(&mut input)
            .context("read SQL from stdin")?;
    } else {
        std::fs::File::open(path)
            .with_context(|| format!("open SQL file {path}"))?
            .take(MAX_SQL_BYTES + 1)
            .read_to_string(&mut input)
            .with_context(|| format!("read SQL file {path}"))?;
    }
    anyhow::ensure!(
        input.len() as u64 <= MAX_SQL_BYTES,
        "SQL input exceeds the {MAX_SQL_BYTES}-byte limit"
    );
    anyhow::ensure!(!input.trim().is_empty(), "SQL statement must not be empty");
    Ok(input)
}

fn resolve_query_mounts(
    mounts: &[String],
    settings_override: Option<&Path>,
) -> Result<Vec<String>> {
    mounts
        .iter()
        .map(|mount| {
            let (name, dataset) = mount
                .split_once('=')
                .context("--mount must use NAME=DATASET")?;
            anyhow::ensure!(!name.is_empty(), "--mount name must not be empty");
            let dataset = expand_dataset_reference(dataset, settings_override, true)?;
            Ok(format!("{name}={dataset}"))
        })
        .collect()
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
                .context("--mount must use NAME=DATASET")?;
            anyhow::ensure!(!name.is_empty(), "--mount name must not be empty");
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
