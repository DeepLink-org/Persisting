//! Public pPilot query commands backed by pChronicle.

use std::collections::HashSet;
use std::fs;
use std::future::Future;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Args, Subcommand, ValueEnum};
use persisting_pchronicle::{
    ChronicleFormat, ChronicleQueryEngine, ChronicleQueryExecutionOptions, ExternalTableFormat,
    ExternalTableSpec, FileTrajectoryDataSourceOptions, LocalQueryManifest,
    LocalQueryManifestOptions, RawEventLanceStore, StoryCoords, StorylineLanceStore,
    DEFAULT_LOCAL_QUERY_BATCH_SIZE, DEFAULT_LOCAL_QUERY_CACHE_BYTES,
    DEFAULT_LOCAL_QUERY_CACHE_FILES, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES,
};

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum QuerySource {
    #[default]
    Auto,
    Lance,
    Atif,
    #[value(name = "openai_msg", alias = "openai-msg")]
    OpenaiMsg,
    Actf,
}

#[derive(Debug, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    group(
        ArgGroup::new("legacy_sql_input")
            .required(true)
            .multiple(false)
            .args(["sql", "sql_file"])
    )
)]
pub struct QueryArgs {
    /// Explicit query mode. Omit for compatibility with `query INPUT --sql ...`.
    #[command(subcommand)]
    pub command: Option<QueryCommand>,

    /// Legacy SQL input: Lance store path/S3 URI, or a supported trajectory file/directory.
    #[arg(value_name = "INPUT", required = true)]
    pub input: Option<String>,

    /// Legacy SQL input representation.
    #[arg(long, value_enum, default_value_t = QuerySource::Auto)]
    pub source: QuerySource,

    /// Legacy SQL external table. Repeat as needed.
    #[arg(long = "table", value_name = "NAME=FORMAT:PATH")]
    pub tables: Vec<String>,

    /// Legacy inline SQL.
    #[arg(long, short = 'q', value_name = "SQL", conflicts_with = "sql_file")]
    pub sql: Option<String>,

    /// Legacy SQL file; use `-` for stdin.
    #[arg(long, value_name = "FILE", conflicts_with = "sql")]
    pub sql_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum QueryCommand {
    /// Run one read-only SQL statement against Lance, ATIF, OpenAI JSON, or ACTF.
    Sql(SqlQueryArgs),
    /// Fetch one normalized step or one complete Storyline.
    Point(PointQueryArgs),
    /// Fetch multiple normalized steps or Storylines in one snapshot.
    Batch(BatchQueryArgs),
    /// Follow newly committed canonical events as JSONL.
    Follow(FollowQueryArgs),
}

#[derive(Debug, Args)]
pub struct SqlQueryArgs {
    /// Lance store path/S3 URI, or a supported trajectory file or directory.
    #[arg(value_name = "INPUT")]
    pub input: String,

    /// Input representation. `auto` detects Lance, ATIF, OpenAI JSON, or ACTF.
    #[arg(long, value_enum, default_value_t = QuerySource::Auto)]
    pub source: QuerySource,

    /// Register an external table before querying. Repeat as needed.
    /// Formats: `NAME=csv:PATH`, `NAME=json:PATH`, `NAME=jsonl:PATH`.
    #[arg(long = "table", value_name = "NAME=FORMAT:PATH")]
    pub tables: Vec<String>,

    /// SQL to execute against runs, steps, and tool_calls.
    #[arg(
        long,
        short = 'q',
        value_name = "SQL",
        required_unless_present = "sql_file",
        conflicts_with = "sql_file"
    )]
    pub sql: Option<String>,

    /// Read SQL from a UTF-8 file; use `-` for stdin.
    #[arg(
        long,
        value_name = "FILE",
        required_unless_present = "sql",
        conflicts_with = "sql"
    )]
    pub sql_file: Option<PathBuf>,

    /// Maximum files admitted to a recursively expanded local manifest.
    #[arg(long, default_value_t = DEFAULT_MAX_LOCAL_QUERY_FILES)]
    pub max_files: usize,

    /// Maximum recursive directory entries visited while building a local manifest.
    #[arg(long, default_value_t = DEFAULT_MAX_LOCAL_QUERY_ENTRIES)]
    pub max_entries: usize,

    /// Maximum bytes accepted for one local trajectory file.
    #[arg(long, default_value_t = DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES)]
    pub max_file_bytes: u64,

    /// Maximum local files parsed concurrently. Defaults to at most 8 CPUs.
    #[arg(long, value_name = "COUNT")]
    pub max_concurrent_files: Option<usize>,

    /// Maximum Arrow bytes retained by the shared parsed-file cache; 0 disables it.
    #[arg(long, default_value_t = DEFAULT_LOCAL_QUERY_CACHE_BYTES)]
    pub cache_bytes: usize,

    /// Maximum files retained by the shared parsed-file cache; 0 disables it.
    #[arg(long, default_value_t = DEFAULT_LOCAL_QUERY_CACHE_FILES)]
    pub cache_files: usize,

    /// Maximum normalized rows per Arrow batch.
    #[arg(long, default_value_t = DEFAULT_LOCAL_QUERY_BATCH_SIZE)]
    pub batch_size: usize,

    /// Emit local-file cache and parsing metrics as JSON to stderr.
    #[arg(long)]
    pub query_metrics: bool,

    /// Abort SQL execution after this many seconds.
    #[arg(long, value_name = "SECONDS")]
    pub timeout_seconds: Option<u64>,

    /// DataFusion operator memory pool size; spillable operators use disk after pressure.
    #[arg(long, value_name = "BYTES")]
    pub memory_limit_bytes: Option<usize>,

    /// Existing directory used for DataFusion spill files.
    #[arg(long, value_name = "DIR")]
    pub spill_path: Option<PathBuf>,

    /// Maximum bytes DataFusion may use in its spill directory.
    #[arg(long, value_name = "BYTES")]
    pub max_spill_bytes: Option<u64>,

    /// Abort after the result exceeds this many rows.
    #[arg(long, value_name = "ROWS")]
    pub max_output_rows: Option<u64>,
}

#[derive(Debug, Args)]
pub struct PointQueryArgs {
    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,
    /// Storyline session id.
    #[arg(long, value_name = "ID")]
    pub session_id: String,
    /// When set, return this normalized step instead of the complete Storyline.
    #[arg(long)]
    pub step_id: Option<i64>,
}

#[derive(Debug, Args)]
pub struct BatchQueryArgs {
    /// Storyline Lance store path or object-store URI.
    #[arg(value_name = "STORE")]
    pub store: String,
    /// Storyline session ids. Repeat or pass a comma-separated list.
    #[arg(
        long = "session-id",
        value_name = "ID",
        value_delimiter = ',',
        required = true
    )]
    pub session_ids: Vec<String>,
    /// When set, return this normalized step from every Storyline.
    #[arg(long)]
    pub step_id: Option<i64>,
}

#[derive(Debug, Args)]
pub struct FollowQueryArgs {
    /// Canonical trajectory storage root.
    #[arg(value_name = "STORAGE")]
    pub storage: String,
    #[arg(long, value_name = "ID")]
    pub agent_id: String,
    #[arg(long, value_name = "ID")]
    pub session_id: String,
    /// Parent Run id for a nested subagent Storyline.
    #[arg(long, value_name = "ID")]
    pub root_session_id: Option<String>,
    /// Number of existing events to skip before replaying and following.
    #[arg(long, default_value_t = 0)]
    pub offset: usize,
    /// Maximum records read per page.
    #[arg(long, default_value_t = 256)]
    pub limit: usize,
    /// Delay between empty polls.
    #[arg(long, default_value_t = 100, value_name = "MILLISECONDS")]
    pub poll_interval_ms: u64,
}

impl QuerySource {
    #[cfg(test)]
    fn resolve(self, input: &str) -> Result<ResolvedQuerySource> {
        self.resolve_with_options(input, LocalQueryManifestOptions::default())
    }

    fn resolve_with_options(
        self,
        input: &str,
        manifest_options: LocalQueryManifestOptions,
    ) -> Result<ResolvedQuerySource> {
        let path = Path::new(input);
        match self {
            Self::Auto if is_lance_object_store_uri(input) => Ok(ResolvedQuerySource::Lance),
            Self::Auto if path.join("CURRENT").is_file() => Ok(ResolvedQuerySource::Lance),
            Self::Auto if path.exists() => Ok(ResolvedQuerySource::Local(
                LocalQueryManifest::detect_with_options(path, manifest_options)?,
            )),
            Self::Auto => bail!("query input does not exist: {input}"),
            Self::Lance => Ok(ResolvedQuerySource::Lance),
            Self::Atif => Ok(ResolvedQuerySource::Local(
                LocalQueryManifest::for_format_with_options(
                    path,
                    ChronicleFormat::Atif,
                    manifest_options,
                )?,
            )),
            Self::OpenaiMsg => Ok(ResolvedQuerySource::Local(
                LocalQueryManifest::for_format_with_options(
                    path,
                    ChronicleFormat::OpenaiMsg,
                    manifest_options,
                )?,
            )),
            Self::Actf => Ok(ResolvedQuerySource::Local(
                LocalQueryManifest::for_format_with_options(
                    path,
                    ChronicleFormat::Actf,
                    manifest_options,
                )?,
            )),
        }
    }
}

#[derive(Debug)]
enum ResolvedQuerySource {
    Lance,
    Local(LocalQueryManifest),
}

fn is_lance_object_store_uri(input: &str) -> bool {
    let Some((scheme, _)) = input.split_once("://") else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "s3" | "az" | "gs" | "memory" | "shared-memory" | "file"
    )
}

/// Execute one pChronicle-backed query mode and write JSONL to stdout.
pub async fn run_query(args: QueryArgs) -> Result<()> {
    let QueryArgs {
        command,
        input,
        source,
        tables,
        sql,
        sql_file,
    } = args;
    match command {
        Some(QueryCommand::Sql(args)) => run_sql_query(args).await,
        Some(QueryCommand::Point(args)) => run_point_query(args).await,
        Some(QueryCommand::Batch(args)) => run_batch_query(args).await,
        Some(QueryCommand::Follow(args)) => run_follow_query(args).await,
        None => {
            let input = input.context(
                "missing query mode or legacy INPUT; use `query sql|point|batch|follow ...`",
            )?;
            run_sql_query(SqlQueryArgs {
                input,
                source,
                tables,
                sql,
                sql_file,
                max_files: DEFAULT_MAX_LOCAL_QUERY_FILES,
                max_entries: DEFAULT_MAX_LOCAL_QUERY_ENTRIES,
                max_file_bytes: DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
                max_concurrent_files: None,
                cache_bytes: DEFAULT_LOCAL_QUERY_CACHE_BYTES,
                cache_files: DEFAULT_LOCAL_QUERY_CACHE_FILES,
                batch_size: DEFAULT_LOCAL_QUERY_BATCH_SIZE,
                query_metrics: false,
                timeout_seconds: None,
                memory_limit_bytes: None,
                spill_path: None,
                max_spill_bytes: None,
                max_output_rows: None,
            })
            .await
        }
    }
}

async fn run_sql_query(args: SqlQueryArgs) -> Result<()> {
    anyhow::ensure!(
        args.timeout_seconds != Some(0),
        "--timeout-seconds must be greater than zero"
    );
    anyhow::ensure!(
        args.max_output_rows != Some(0),
        "--max-output-rows must be greater than zero"
    );
    let sql = read_sql(&args)?;
    let external_tables = parse_external_tables(&args.tables)?;
    let manifest_options = LocalQueryManifestOptions {
        max_files: args.max_files,
        max_entries: args.max_entries,
        max_detection_bytes: args.max_file_bytes,
    };
    let mut file_options = FileTrajectoryDataSourceOptions {
        batch_size: args.batch_size,
        max_file_bytes: args.max_file_bytes,
        cache_bytes: args.cache_bytes,
        cache_files: args.cache_files,
        ..FileTrajectoryDataSourceOptions::default()
    };
    if let Some(max_concurrent_files) = args.max_concurrent_files {
        file_options.max_concurrent_files = max_concurrent_files;
    }
    let execution_options = ChronicleQueryExecutionOptions {
        memory_limit_bytes: args.memory_limit_bytes,
        spill_path: args.spill_path.clone(),
        max_spill_bytes: args.max_spill_bytes,
    };
    let engine = match args
        .source
        .resolve_with_options(&args.input, manifest_options)?
    {
        ResolvedQuerySource::Lance => ChronicleQueryEngine::open_lance_uri_with_options(
            &args.input,
            execution_options.clone(),
        )
        .await
        .with_context(|| format!("open Lance store {}", args.input))?,
        ResolvedQuerySource::Local(manifest) => {
            ChronicleQueryEngine::open_local_manifest_with_execution_options(
                manifest,
                file_options,
                execution_options,
            )
            .with_context(|| format!("open local trajectory input {}", args.input))?
        }
    };
    for table in &external_tables {
        engine.register_external_table(table).await?;
    }
    let stdout = io::stdout();
    let execution =
        engine.write_query_jsonl_with_max_rows(&sql, stdout.lock(), args.max_output_rows);
    let execution_result = if let Some(seconds) = args.timeout_seconds {
        tokio::time::timeout(Duration::from_secs(seconds), execution)
            .await
            .with_context(|| format!("SQL query timed out after {seconds} seconds"))?
    } else {
        execution.await
    };
    if let Err(error) = execution_result {
        if error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<io::Error>())
            .any(|error| error.kind() == io::ErrorKind::BrokenPipe)
        {
            return Ok(());
        }
        return Err(error);
    }
    if args.query_metrics {
        if let Some(metrics) = engine.local_file_metrics() {
            writeln!(io::stderr().lock(), "{}", serde_json::to_string(&metrics)?)
                .context("write local query metrics to stderr")?;
        }
    }
    Ok(())
}

async fn run_point_query(args: PointQueryArgs) -> Result<()> {
    anyhow::ensure!(
        !args.session_id.is_empty(),
        "--session-id must not be empty"
    );
    if let Some(step_id) = args.step_id {
        let engine = ChronicleQueryEngine::open_lance_uri(&args.store)
            .await
            .with_context(|| format!("open Storyline Lance store {}", args.store))?;
        let sql = format!(
            "SELECT * FROM steps WHERE session_id = {} AND step_id = {step_id}",
            sql_string(&args.session_id)
        );
        let output = engine.query_jsonl(&sql).await?;
        let row_count = output.lines().filter(|line| !line.is_empty()).count();
        anyhow::ensure!(
            row_count == 1,
            "point step {}/{} returned {row_count} rows",
            args.session_id,
            step_id
        );
        return write_stdout(output.as_bytes());
    }

    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let story = store
        .get_storyline(&args.session_id)
        .await?
        .with_context(|| format!("Storyline session '{}' was not found", args.session_id))?;
    write_jsonl_values(std::slice::from_ref(&story))
}

async fn run_batch_query(args: BatchQueryArgs) -> Result<()> {
    validate_batch_session_ids(&args.session_ids)?;
    if let Some(step_id) = args.step_id {
        let engine = ChronicleQueryEngine::open_lance_uri(&args.store)
            .await
            .with_context(|| format!("open Storyline Lance store {}", args.store))?;
        let predicate = args
            .session_ids
            .iter()
            .map(|session_id| sql_string(session_id))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT * FROM steps WHERE session_id IN ({predicate}) AND step_id = {step_id} ORDER BY session_id"
        );
        let output = engine.query_jsonl(&sql).await?;
        ensure_batch_step_ids(&args.session_ids, &output, step_id)?;
        return write_stdout(output.as_bytes());
    }

    let store = StorylineLanceStore::open_uri(&args.store)
        .await
        .with_context(|| format!("open Storyline Lance store {}", args.store))?;
    let stories = store.get_storylines(&args.session_ids).await?;
    let missing = args
        .session_ids
        .iter()
        .zip(&stories)
        .filter_map(|(session_id, story)| story.is_none().then_some(session_id.as_str()))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "Storyline sessions not found: {}",
        missing.join(", ")
    );
    let stories = stories.into_iter().flatten().collect::<Vec<_>>();
    write_jsonl_values(&stories)
}

async fn run_follow_query(args: FollowQueryArgs) -> Result<()> {
    anyhow::ensure!(args.limit > 0, "--limit must be greater than zero");
    anyhow::ensure!(
        args.poll_interval_ms > 0,
        "--poll-interval-ms must be greater than zero"
    );
    let session = StoryCoords::new(
        args.storage,
        args.agent_id,
        args.session_id,
        args.root_session_id,
    );
    eprintln!(
        "[ppilot] following {}/{}/{} from offset {} every {} ms",
        session.storage, session.agent_id, session.session_id, args.offset, args.poll_interval_ms
    );
    let stdout = io::stdout();
    let mut output = stdout.lock();
    follow_query_jsonl(
        &session,
        args.offset,
        args.limit,
        Duration::from_millis(args.poll_interval_ms),
        &mut output,
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
}

async fn follow_query_jsonl<W, S>(
    session: &StoryCoords,
    mut offset: usize,
    page_size: usize,
    poll_interval: Duration,
    output: &mut W,
    shutdown: S,
) -> Result<()>
where
    W: Write,
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    let store = RawEventLanceStore;
    loop {
        let page = tokio::select! {
            _ = &mut shutdown => return Ok(()),
            page = store.replay_available(session, offset, Some(page_size)) => page?,
        };
        let records = page.map(|page| page.records).unwrap_or_default();
        if !records.is_empty() {
            for record in &records {
                if let Err(error) = output
                    .write_all(record.trim_end().as_bytes())
                    .and_then(|_| output.write_all(b"\n"))
                {
                    if error.kind() == io::ErrorKind::BrokenPipe {
                        return Ok(());
                    }
                    return Err(error).context("write followed pChronicle JSONL");
                }
            }
            if let Err(error) = output.flush() {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    return Ok(());
                }
                return Err(error).context("flush followed pChronicle JSONL");
            }
            offset = offset.saturating_add(records.len());
            continue;
        }
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(poll_interval) => {}
        }
    }
}

fn validate_batch_session_ids(session_ids: &[String]) -> Result<()> {
    anyhow::ensure!(!session_ids.is_empty(), "provide at least one --session-id");
    anyhow::ensure!(
        session_ids.iter().all(|session_id| !session_id.is_empty()),
        "--session-id must not be empty"
    );
    let unique = session_ids.iter().collect::<HashSet<_>>();
    anyhow::ensure!(
        unique.len() == session_ids.len(),
        "duplicate --session-id in point batch"
    );
    Ok(())
}

fn ensure_batch_step_ids(session_ids: &[String], output: &str, step_id: i64) -> Result<()> {
    let returned = output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(line).context("decode batch step JSONL")?;
            value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .context("batch step row is missing session_id")
        })
        .collect::<Result<HashSet<_>>>()?;
    let missing = session_ids
        .iter()
        .filter(|session_id| !returned.contains(session_id.as_str()))
        .map(String::as_str)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty() && returned.len() == session_ids.len(),
        "step {step_id} was not found in sessions: {}",
        missing.join(", ")
    );
    Ok(())
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn write_jsonl_values<T: serde::Serialize>(values: &[T]) -> Result<()> {
    let mut output = Vec::new();
    for value in values {
        serde_json::to_writer(&mut output, value).context("encode pChronicle JSONL")?;
        output.push(b'\n');
    }
    write_stdout(&output)
}

fn write_stdout(output: &[u8]) -> Result<()> {
    match io::stdout().lock().write_all(output) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("write query JSONL to stdout"),
    }
}

fn parse_external_tables(values: &[String]) -> Result<Vec<ExternalTableSpec>> {
    let mut names = HashSet::with_capacity(values.len());
    values
        .iter()
        .map(|value| {
            let (name, source) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("invalid --table {value:?}; expected NAME=FORMAT:PATH")
            })?;
            let (format, path) = source.split_once(':').ok_or_else(|| {
                anyhow::anyhow!("invalid --table {value:?}; expected NAME=FORMAT:PATH")
            })?;
            let name = name.trim();
            let path = path.trim();
            if name.is_empty() || path.is_empty() {
                bail!("invalid --table {value:?}; NAME and PATH must not be empty");
            }
            let format = match format.trim().to_ascii_lowercase().as_str() {
                "csv" => ExternalTableFormat::Csv,
                "json" => ExternalTableFormat::Json,
                "jsonl" | "ndjson" => ExternalTableFormat::JsonLines,
                other => bail!(
                    "unsupported --table format {other:?}; expected csv, json, jsonl, or ndjson"
                ),
            };
            if !names.insert(name.to_string()) {
                bail!("duplicate --table name {name:?}");
            }
            Ok(ExternalTableSpec::new(name, format, path))
        })
        .collect()
}

fn read_sql(args: &SqlQueryArgs) -> Result<String> {
    match (&args.sql, &args.sql_file) {
        (Some(sql), None) => Ok(sql.clone()),
        (None, Some(path)) if path == Path::new("-") => {
            let mut sql = String::new();
            io::stdin()
                .read_to_string(&mut sql)
                .context("read SQL from stdin")?;
            Ok(sql)
        }
        (None, Some(path)) => {
            fs::read_to_string(path).with_context(|| format!("read SQL file {}", path.display()))
        }
        _ => bail!("provide exactly one of --sql or --sql-file"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_detects_lance_and_supported_json_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let lance = temp.path().join("lance");
        fs::create_dir(&lance)?;
        fs::write(lance.join("CURRENT"), "gen-test\n")?;
        let atif = temp.path().join("input.jsonl");
        fs::write(
            &atif,
            r#"{"schema_version":"ATIF-v1.4","steps":[],"agent":{}}"#,
        )?;
        let openai = temp.path().join("openai.json");
        fs::write(
            &openai,
            r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
        )?;
        let actf = temp.path().join("task.json");
        fs::write(
            &actf,
            r#"{"attempts":{"a":{"trajectory":{"schema_version":"ACTF_1.0"}}}}"#,
        )?;

        assert!(matches!(
            QuerySource::Auto.resolve(lance.to_str().unwrap())?,
            ResolvedQuerySource::Lance
        ));
        assert_eq!(
            resolved_format(QuerySource::Auto.resolve(atif.to_str().unwrap())?),
            Some(ChronicleFormat::Atif)
        );
        assert_eq!(
            resolved_format(QuerySource::Auto.resolve(openai.to_str().unwrap())?),
            Some(ChronicleFormat::OpenaiMsg)
        );
        assert_eq!(
            resolved_format(QuerySource::Auto.resolve(actf.to_str().unwrap())?),
            Some(ChronicleFormat::Actf)
        );
        for uri in [
            "s3://trajectory-bucket/storylines",
            "S3://trajectory-bucket/storylines",
            "az://container/storylines",
            "gs://bucket/storylines",
            "memory://storylines",
            "shared-memory://process/storylines",
            "file:///tmp/storylines",
        ] {
            assert!(
                matches!(QuerySource::Auto.resolve(uri)?, ResolvedQuerySource::Lance),
                "expected Lance auto-detection for {uri}"
            );
        }
        Ok(())
    }

    #[test]
    fn auto_freezes_recursive_manifest_and_defers_file_validation() -> Result<()> {
        let openai = tempfile::tempdir()?;
        fs::write(
            openai.path().join("01.json"),
            r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
        )?;
        fs::create_dir(openai.path().join("nested"))?;
        fs::write(
            openai.path().join("nested/02.json"),
            r#"[{"session_id":"s2","step_id":0,"messages":[]}]"#,
        )?;
        assert_eq!(
            resolved_format(QuerySource::Auto.resolve(openai.path().to_str().unwrap())?),
            Some(ChronicleFormat::OpenaiMsg)
        );

        fs::write(
            openai.path().join("task.actf.json"),
            r#"{"attempts":{"a":{"trajectory":{"schema_version":"ACTF_1.0"}}}}"#,
        )?;
        let resolved = QuerySource::Auto.resolve(openai.path().to_str().unwrap())?;
        assert_eq!(resolved_format(resolved), Some(ChronicleFormat::OpenaiMsg));
        Ok(())
    }

    #[test]
    fn auto_rejects_unknown_uri_and_missing_local_input() {
        for input in [
            "https://example.com/storylines",
            "/definitely/missing/input",
        ] {
            let error = QuerySource::Auto.resolve(input).unwrap_err();
            assert!(error.to_string().contains("does not exist"), "{error:#}");
        }
        assert!(matches!(
            QuerySource::Lance
                .resolve("https://example.com/explicit")
                .unwrap(),
            ResolvedQuerySource::Lance
        ));
    }

    fn resolved_format(source: ResolvedQuerySource) -> Option<ChronicleFormat> {
        match source {
            ResolvedQuerySource::Lance => None,
            ResolvedQuerySource::Local(manifest) => Some(manifest.format()),
        }
    }

    #[test]
    fn reads_inline_and_file_sql() -> Result<()> {
        let inline = SqlQueryArgs {
            input: "ignored".into(),
            source: QuerySource::Auto,
            tables: Vec::new(),
            sql: Some("SELECT 1".into()),
            sql_file: None,
            max_files: DEFAULT_MAX_LOCAL_QUERY_FILES,
            max_entries: DEFAULT_MAX_LOCAL_QUERY_ENTRIES,
            max_file_bytes: DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
            max_concurrent_files: None,
            cache_bytes: DEFAULT_LOCAL_QUERY_CACHE_BYTES,
            cache_files: DEFAULT_LOCAL_QUERY_CACHE_FILES,
            batch_size: DEFAULT_LOCAL_QUERY_BATCH_SIZE,
            query_metrics: false,
            timeout_seconds: None,
            memory_limit_bytes: None,
            spill_path: None,
            max_spill_bytes: None,
            max_output_rows: None,
        };
        assert_eq!(read_sql(&inline)?, "SELECT 1");

        let temp = tempfile::NamedTempFile::new()?;
        fs::write(temp.path(), "SELECT * FROM steps")?;
        let file = SqlQueryArgs {
            input: "ignored".into(),
            source: QuerySource::Auto,
            tables: Vec::new(),
            sql: None,
            sql_file: Some(temp.path().to_owned()),
            max_files: DEFAULT_MAX_LOCAL_QUERY_FILES,
            max_entries: DEFAULT_MAX_LOCAL_QUERY_ENTRIES,
            max_file_bytes: DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
            max_concurrent_files: None,
            cache_bytes: DEFAULT_LOCAL_QUERY_CACHE_BYTES,
            cache_files: DEFAULT_LOCAL_QUERY_CACHE_FILES,
            batch_size: DEFAULT_LOCAL_QUERY_BATCH_SIZE,
            query_metrics: false,
            timeout_seconds: None,
            memory_limit_bytes: None,
            spill_path: None,
            max_spill_bytes: None,
            max_output_rows: None,
        };
        assert_eq!(read_sql(&file)?, "SELECT * FROM steps");
        Ok(())
    }

    #[test]
    fn parses_repeatable_external_tables_and_preserves_uri_colons() -> Result<()> {
        let tables = parse_external_tables(&[
            "labels=csv:/tmp/labels.csv".into(),
            "metadata=json:s3://bucket/metadata.json".into(),
            "events=ndjson:/tmp/events.ndjson".into(),
        ])?;

        assert_eq!(
            tables,
            vec![
                ExternalTableSpec::new("labels", ExternalTableFormat::Csv, "/tmp/labels.csv"),
                ExternalTableSpec::new(
                    "metadata",
                    ExternalTableFormat::Json,
                    "s3://bucket/metadata.json"
                ),
                ExternalTableSpec::new(
                    "events",
                    ExternalTableFormat::JsonLines,
                    "/tmp/events.ndjson"
                ),
            ]
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_external_table_specs() {
        for value in [
            "missing-format",
            "=csv:/tmp/labels.csv",
            "labels=csv:",
            "labels=parquet:/tmp/labels.parquet",
            "labels=csv:/tmp/a.csv,labels=csv:/tmp/b.csv",
        ] {
            let values = if value.contains(',') {
                value.split(',').map(str::to_owned).collect::<Vec<_>>()
            } else {
                vec![value.to_owned()]
            };
            assert!(parse_external_tables(&values).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn query_help_documents_repeatable_external_tables() {
        let mut command = SqlQueryArgs::augment_args(clap::Command::new("sql"));
        let help = command.render_long_help().to_string();
        assert!(help.contains("--table <NAME=FORMAT:PATH>"), "{help}");
        assert!(help.contains("NAME=jsonl:PATH"), "{help}");
        assert!(help.contains("openai_msg"), "{help}");
        assert!(help.contains("actf"), "{help}");
    }

    #[test]
    fn validates_batch_ids_and_escapes_sql_strings() {
        validate_batch_session_ids(&["a".into(), "b".into()]).unwrap();
        assert!(validate_batch_session_ids(&["a".into(), "a".into()]).is_err());
        assert_eq!(sql_string("agent's-run"), "'agent''s-run'");
    }
}
