//! Public SQL query engine over Lance, ATIF, OpenAI JSON, or ACTF sources.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use datafusion::arrow::json::LineDelimitedWriter;
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::dataframe::DataFrame;
use datafusion::execution::memory_pool::FairSpillPool;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::logical_expr::{Expr, LogicalPlan};
use datafusion::prelude::{CsvReadOptions, JsonReadOptions, SessionConfig, SessionContext};
use datafusion::sql::parser::{DFParser, Statement as DataFusionStatement};
use datafusion::sql::sqlparser::ast::Statement as SqlStatement;
use futures::TryStreamExt;

use super::{
    AtifDataSource, FileTrajectoryDataSource, FileTrajectoryDataSourceOptions,
    FileTrajectoryFormat, FileTrajectoryQueryMetrics, FileTrajectoryQueryMetricsSnapshot,
    LocalQueryManifest, StorylineDataSource, SOURCE_FILE_COLUMN,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChronicleQueryBackend {
    Lance {
        generation: String,
    },
    Atif {
        files: usize,
        documents: Option<usize>,
        steps: Option<usize>,
        tool_calls: Option<usize>,
    },
    OpenaiMsg {
        files: usize,
    },
    Actf {
        files: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTableFormat {
    Csv,
    /// One JSON array containing zero or more objects.
    Json,
    /// Newline-delimited JSON (`.jsonl` or `.ndjson`).
    JsonLines,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalTableSpec {
    pub name: String,
    pub format: ExternalTableFormat,
    pub path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChronicleQueryExecutionOptions {
    /// DataFusion operator memory pool. `None` retains DataFusion's default.
    pub memory_limit_bytes: Option<usize>,
    /// Directory used by spillable operators. `None` uses DataFusion's default.
    pub spill_path: Option<PathBuf>,
    /// Maximum bytes permitted in the spill directory.
    pub max_spill_bytes: Option<u64>,
}

impl ExternalTableSpec {
    pub fn new(
        name: impl Into<String>,
        format: ExternalTableFormat,
        path: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            format,
            path: path.into(),
        }
    }
}

/// Read-only SQL engine exposing the same normalized tables for all sources.
pub struct ChronicleQueryEngine {
    context: SessionContext,
    backend: ChronicleQueryBackend,
    require_file_join_key: bool,
    local_file_metrics: Option<FileTrajectoryQueryMetrics>,
}

impl std::fmt::Debug for ChronicleQueryEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChronicleQueryEngine")
            .field("backend", &self.backend)
            .finish_non_exhaustive()
    }
}

impl ChronicleQueryEngine {
    pub async fn open_lance(root: impl AsRef<Path>) -> Result<Self> {
        let source = StorylineDataSource::open(root).await?;
        Self::from_lance_source(source)
    }

    /// Open a Lance store from a local path or object-store URI such as S3.
    pub async fn open_lance_uri(root: impl AsRef<str>) -> Result<Self> {
        Self::open_lance_uri_with_options(root, ChronicleQueryExecutionOptions::default()).await
    }

    pub async fn open_lance_uri_with_options(
        root: impl AsRef<str>,
        options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        let source = StorylineDataSource::open_uri(root).await?;
        Self::from_lance_source_with_options(source, options)
    }

    pub fn open_atif(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file_trajectory_source(FileTrajectoryDataSource::open_atif(path)?)
    }

    pub fn open_openai_msg(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file_trajectory_source(FileTrajectoryDataSource::open_openai_msg(path)?)
    }

    pub fn open_actf(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_file_trajectory_source(FileTrajectoryDataSource::open_actf(path)?)
    }

    pub fn open_local_manifest(manifest: LocalQueryManifest) -> Result<Self> {
        Self::open_local_manifest_with_options(manifest, FileTrajectoryDataSourceOptions::default())
    }

    pub fn open_local_manifest_with_options(
        manifest: LocalQueryManifest,
        options: FileTrajectoryDataSourceOptions,
    ) -> Result<Self> {
        Self::open_local_manifest_with_execution_options(
            manifest,
            options,
            ChronicleQueryExecutionOptions::default(),
        )
    }

    pub fn open_local_manifest_with_execution_options(
        manifest: LocalQueryManifest,
        file_options: FileTrajectoryDataSourceOptions,
        execution_options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        Self::from_file_trajectory_source_with_options(
            FileTrajectoryDataSource::from_manifest_with_options(manifest, file_options)?,
            execution_options,
        )
    }

    pub fn from_lance_source(source: StorylineDataSource) -> Result<Self> {
        Self::from_lance_source_with_options(source, ChronicleQueryExecutionOptions::default())
    }

    pub fn from_lance_source_with_options(
        source: StorylineDataSource,
        options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        let generation = source.generation().to_string();
        let context = query_session_context(&options)?;
        source.register(&context)?;
        Ok(Self {
            context,
            backend: ChronicleQueryBackend::Lance { generation },
            require_file_join_key: false,
            local_file_metrics: None,
        })
    }

    pub fn from_atif_source(source: AtifDataSource) -> Result<Self> {
        let files = source.file_count();
        let backend = ChronicleQueryBackend::Atif {
            files,
            documents: source.document_count(),
            steps: source.step_count(),
            tool_calls: source.tool_call_count(),
        };
        let context = source.session_context()?;
        Ok(Self {
            context,
            backend,
            require_file_join_key: files > 1,
            local_file_metrics: None,
        })
    }

    pub fn from_file_trajectory_source(source: FileTrajectoryDataSource) -> Result<Self> {
        Self::from_file_trajectory_source_with_options(
            source,
            ChronicleQueryExecutionOptions::default(),
        )
    }

    pub fn from_file_trajectory_source_with_options(
        source: FileTrajectoryDataSource,
        options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        let files = source.file_count();
        let metrics = source.metrics();
        let backend = match source.format() {
            FileTrajectoryFormat::Atif => ChronicleQueryBackend::Atif {
                files,
                documents: None,
                steps: None,
                tool_calls: None,
            },
            FileTrajectoryFormat::OpenaiMsg => ChronicleQueryBackend::OpenaiMsg { files },
            FileTrajectoryFormat::Actf => ChronicleQueryBackend::Actf { files },
        };
        let context = query_session_context(&options)?;
        source.register(&context)?;
        Ok(Self {
            context,
            backend,
            require_file_join_key: files > 1,
            local_file_metrics: Some(metrics),
        })
    }

    pub fn context(&self) -> &SessionContext {
        &self.context
    }

    pub fn backend(&self) -> &ChronicleQueryBackend {
        &self.backend
    }

    pub fn local_file_metrics(&self) -> Option<FileTrajectoryQueryMetricsSnapshot> {
        self.local_file_metrics
            .as_ref()
            .map(FileTrajectoryQueryMetrics::snapshot)
    }

    /// Register a read-only file source in the same DataFusion context as the
    /// normalized `runs`, `steps`, and `tool_calls` tables.
    pub async fn register_external_table(&self, spec: &ExternalTableSpec) -> Result<()> {
        validate_external_table_spec(spec)?;
        anyhow::ensure!(
            !self
                .context
                .table_exist(spec.name.as_str())
                .context("inspect DataFusion table catalog")?,
            "DataFusion table '{}' is already registered",
            spec.name
        );
        match spec.format {
            ExternalTableFormat::Csv => {
                self.context
                    .register_csv(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        CsvReadOptions::new(),
                    )
                    .await
            }
            ExternalTableFormat::Json => {
                self.context
                    .register_json(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        JsonReadOptions::default().newline_delimited(false),
                    )
                    .await
            }
            ExternalTableFormat::JsonLines => {
                let extension = json_lines_extension(&spec.path);
                self.context
                    .register_json(
                        spec.name.as_str(),
                        spec.path.as_str(),
                        JsonReadOptions::default().file_extension(extension),
                    )
                    .await
            }
        }
        .with_context(|| {
            format!(
                "register external DataFusion table '{}' from {}",
                spec.name, spec.path
            )
        })
    }

    /// Build a lazy DataFusion DataFrame for callers that need plan inspection
    /// or further DataFrame transformations.
    pub async fn dataframe(&self, sql: &str) -> Result<DataFrame> {
        ensure_read_only_query(sql)?;
        let dataframe = self
            .context
            .sql(sql)
            .await
            .with_context(|| format!("plan pChronicle SQL: {sql}"))?;
        if self.require_file_join_key {
            ensure_collision_safe_file_joins(dataframe.logical_plan())?;
        }
        Ok(dataframe)
    }

    /// Execute SQL and collect Arrow record batches.
    pub async fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        self.dataframe(sql)
            .await?
            .collect()
            .await
            .with_context(|| format!("execute pChronicle SQL: {sql}"))
    }

    /// Execute SQL and encode result rows as JSONL.
    pub async fn query_jsonl(&self, sql: &str) -> Result<String> {
        let batches = self.query(sql).await?;
        let mut output = Vec::new();
        {
            let mut writer = LineDelimitedWriter::new(&mut output);
            for batch in &batches {
                writer
                    .write(batch)
                    .context("encode SQL result batch as JSONL")?;
            }
            writer.finish().context("finish SQL JSONL output")?;
        }
        String::from_utf8(output).context("DataFusion JSONL output is not UTF-8")
    }

    /// Execute SQL and stream JSONL batches to a writer without collecting the
    /// complete result in memory.
    pub async fn write_query_jsonl<W: std::io::Write>(&self, sql: &str, output: W) -> Result<()> {
        self.write_query_jsonl_with_max_rows(sql, output, None)
            .await
    }

    /// Stream JSONL while rejecting results that exceed a caller-provided row
    /// budget. Batches written before the limit is discovered are not rolled back.
    pub async fn write_query_jsonl_with_max_rows<W: std::io::Write>(
        &self,
        sql: &str,
        mut output: W,
        max_rows: Option<u64>,
    ) -> Result<()> {
        if let Some(max_rows) = max_rows {
            anyhow::ensure!(max_rows > 0, "query max_rows must be greater than zero");
        }
        let mut stream = self
            .dataframe(sql)
            .await?
            .execute_stream()
            .await
            .with_context(|| format!("start streaming pChronicle SQL: {sql}"))?;
        let mut writer = LineDelimitedWriter::new(&mut output);
        let mut rows_written = 0u64;
        while let Some(batch) = stream
            .try_next()
            .await
            .with_context(|| format!("stream pChronicle SQL: {sql}"))?
        {
            rows_written = rows_written
                .checked_add(batch.num_rows() as u64)
                .context("streaming SQL result row count overflow")?;
            if let Some(max_rows) = max_rows {
                anyhow::ensure!(
                    rows_written <= max_rows,
                    "SQL result exceeds max_output_rows limit of {max_rows}"
                );
            }
            writer
                .write(&batch)
                .context("encode streaming SQL result batch as JSONL")?;
        }
        writer.finish().context("finish streaming SQL JSONL output")
    }
}

fn query_session_context(options: &ChronicleQueryExecutionOptions) -> Result<SessionContext> {
    if let Some(memory_limit_bytes) = options.memory_limit_bytes {
        anyhow::ensure!(
            memory_limit_bytes > 0,
            "query memory_limit_bytes must be greater than zero"
        );
    }
    if let Some(max_spill_bytes) = options.max_spill_bytes {
        anyhow::ensure!(
            max_spill_bytes > 0,
            "query max_spill_bytes must be greater than zero"
        );
    }
    if let Some(path) = &options.spill_path {
        anyhow::ensure!(
            path.is_dir(),
            "query spill_path is not a directory: {}",
            path.display()
        );
    }

    let mut runtime = RuntimeEnvBuilder::new();
    if let Some(memory_limit_bytes) = options.memory_limit_bytes {
        runtime = runtime.with_memory_pool(Arc::new(FairSpillPool::new(memory_limit_bytes)));
    }
    if let Some(path) = &options.spill_path {
        runtime = runtime.with_temp_file_path(path);
    }
    if let Some(max_spill_bytes) = options.max_spill_bytes {
        runtime = runtime.with_max_temp_directory_size(max_spill_bytes);
    }
    let runtime = Arc::new(runtime.build().context("build DataFusion query runtime")?);
    Ok(SessionContext::new_with_config_rt(
        SessionConfig::new(),
        runtime,
    ))
}

fn validate_external_table_spec(spec: &ExternalTableSpec) -> Result<()> {
    let mut characters = spec.name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    anyhow::ensure!(
        valid_start && valid_rest,
        "external table name '{}' must match [A-Za-z_][A-Za-z0-9_]*",
        spec.name
    );
    anyhow::ensure!(
        !spec.path.trim().is_empty(),
        "external table '{}' path must not be empty",
        spec.name
    );
    Ok(())
}

fn json_lines_extension(path: &str) -> &str {
    let path_without_query = path.split(['?', '#']).next().unwrap_or(path);
    if path_without_query.ends_with(".ndjson") {
        ".ndjson"
    } else {
        ".jsonl"
    }
}

fn ensure_read_only_query(sql: &str) -> Result<()> {
    let statements = DFParser::parse_sql(sql).context("parse pChronicle SQL")?;
    anyhow::ensure!(
        statements.len() == 1,
        "pChronicle query accepts exactly one SQL statement"
    );
    anyhow::ensure!(
        is_read_only_statement(&statements[0]),
        "pChronicle query only accepts SELECT/VALUES/DESCRIBE/EXPLAIN statements"
    );
    Ok(())
}

fn ensure_collision_safe_file_joins(plan: &LogicalPlan) -> Result<()> {
    if let LogicalPlan::Join(join) = plan {
        if plan_uses_local_trajectory_table(&join.left)
            && plan_uses_local_trajectory_table(&join.right)
            && !join
                .on
                .iter()
                .any(|(left, right)| is_source_file_column(left) && is_source_file_column(right))
            && !join
                .filter
                .as_ref()
                .is_some_and(expr_has_source_file_equality)
        {
            anyhow::bail!(
                "multi-file trajectory joins must include left.{0} = right.{0}; session_id is only unique within one source file",
                SOURCE_FILE_COLUMN
            );
        }
    }
    for input in plan.inputs() {
        ensure_collision_safe_file_joins(input)?;
    }
    Ok(())
}

fn plan_uses_local_trajectory_table(plan: &LogicalPlan) -> bool {
    if let LogicalPlan::TableScan(scan) = plan {
        let table = scan.table_name.to_string();
        let table = table.rsplit('.').next().unwrap_or(table.as_str());
        if matches!(table, "runs" | "steps" | "tool_calls") {
            return true;
        }
    }
    plan.inputs()
        .into_iter()
        .any(plan_uses_local_trajectory_table)
}

fn expr_has_source_file_equality(expr: &Expr) -> bool {
    match expr {
        Expr::BinaryExpr(binary)
            if binary.op == datafusion::logical_expr::Operator::Eq
                && is_source_file_column(&binary.left)
                && is_source_file_column(&binary.right) =>
        {
            true
        }
        Expr::BinaryExpr(binary) if binary.op == datafusion::logical_expr::Operator::And => {
            expr_has_source_file_equality(&binary.left)
                || expr_has_source_file_equality(&binary.right)
        }
        _ => false,
    }
}

fn is_source_file_column(expr: &Expr) -> bool {
    matches!(expr, Expr::Column(column) if column.name == SOURCE_FILE_COLUMN)
}

fn is_read_only_statement(statement: &DataFusionStatement) -> bool {
    match statement {
        DataFusionStatement::Statement(statement) => is_read_only_sql_statement(statement),
        DataFusionStatement::Explain(explain) => is_read_only_statement(&explain.statement),
        DataFusionStatement::CreateExternalTable(_)
        | DataFusionStatement::CopyTo(_)
        | DataFusionStatement::Reset(_) => false,
    }
}

fn is_read_only_sql_statement(statement: &SqlStatement) -> bool {
    match statement {
        SqlStatement::Query(_) | SqlStatement::ExplainTable { .. } => true,
        SqlStatement::Explain { statement, .. } => is_read_only_sql_statement(statement),
        _ => false,
    }
}
