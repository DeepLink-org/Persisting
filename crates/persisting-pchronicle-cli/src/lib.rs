use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use persisting_pchronicle::{
    CatalogErrorPolicy, CatalogSnapshotOptions, CatalogSourceKind, CatalogSourceStatus,
    DatasetCatalogSnapshot, DatasetMount, LocalQueryManifestOptions, DEFAULT_DATASET_NAME,
};
use serde::Serialize;
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
    Status(DatasetArgs),
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
struct QueryArgs {
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,
    #[arg(value_name = "SQL")]
    sql: String,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("identity")
        .required(true)
        .multiple(false)
        .args(["run_id", "session_id"])
))]
struct FindArgs {
    #[arg(value_name = "DATASET_URI")]
    dataset_uri: String,
    #[arg(long)]
    source: Option<String>,
    #[arg(long)]
    run_id: Option<String>,
    #[arg(long)]
    session_id: Option<String>,
    #[arg(long, requires = "session_id")]
    step_id: Option<String>,
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

pub async fn run(
    cli: Cli,
    stdout_is_terminal: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Result<()> {
    match cli.command {
        Command::Ls(args) => run_list(args, stdout_is_terminal, stdout, stderr).await,
        Command::Status(args) => not_implemented("status", Some(&args.dataset_uri)),
        Command::Query(args) => {
            let _ = args.sql;
            not_implemented("query", Some(&args.dataset_uri))
        }
        Command::Find(args) => {
            let _ = (args.source, args.run_id, args.session_id, args.step_id);
            not_implemented("find", Some(&args.dataset_uri))
        }
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
    let dataset_uri = normalize_and_validate_dataset_uri(&args.dataset_uri)?;
    let mount = DatasetMount::default(dataset_uri.clone())?;
    let snapshot = DatasetCatalogSnapshot::discover(
        vec![mount],
        Some(DEFAULT_DATASET_NAME.into()),
        CatalogSnapshotOptions {
            error_policy: args.errors.into(),
            manifest: LocalQueryManifestOptions {
                max_files: args.max_files,
                max_entries: args.max_entries,
                ..LocalQueryManifestOptions::default()
            },
            ..CatalogSnapshotOptions::default()
        },
    )
    .await
    .with_context(|| "discover Dataset Sources")?;
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
        writeln!(stdout, "{}", line.trim_end()).context("write pChronicle ls table")?;
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
        let cli = Cli::try_parse_from(["pchronicle", "status", "."])?;
        let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "pchronicle status is not implemented yet"
        );
        Ok(())
    }

    #[test]
    fn byte_units_are_stable() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }
}
