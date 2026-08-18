//! Public SQL query engine over Lance, ATIF, OpenAI JSON, or ACTF sources.

use std::collections::BTreeSet;
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
    datafusion_bridge::from_datafusion, DatasetCatalogSnapshot, FileTrajectoryQueryMetrics,
    FileTrajectoryQueryMetricsSnapshot, SOURCE_FILE_COLUMN,
};
use crate::{DocumentFormat, QueryCapabilities, QueryTables};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryBackendInfo {
    pub format: DocumentFormat,
    pub tables: QueryTables,
    pub capabilities: QueryCapabilities,
    pub source_count: usize,
    pub snapshot: Option<QuerySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuerySnapshot {
    CanonicalEvent {
        format_version: u32,
        fact_version: u64,
        fact_rows: u64,
        layout_revision: u64,
    },
    Storyline {
        generation: String,
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
    backend_info: Option<QueryBackendInfo>,
    require_file_join_key: bool,
    local_file_metrics: Vec<FileTrajectoryQueryMetrics>,
    // Keeps pinned remote-file materializations alive for the complete query.
    _catalog_snapshot: Option<Arc<DatasetCatalogSnapshot>>,
}

impl std::fmt::Debug for ChronicleQueryEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChronicleQueryEngine")
            .field("backend_info", &self.backend_info)
            .finish_non_exhaustive()
    }
}

impl ChronicleQueryEngine {
    pub async fn open(
        format: DocumentFormat,
        path: impl AsRef<Path>,
        options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        let source = crate::document::open_document(format, path.as_ref()).await?;
        let context = query_session_context(&options)?;
        let tables = source.register_datafusion(&context)?;
        let capabilities = source.capabilities();
        let source_count = source.inner.source_count();
        let snapshot = if let Some(snapshot) = source.inner.event_snapshot() {
            Some(QuerySnapshot::CanonicalEvent {
                // Existing canonical manifests predate explicit format versioning.
                format_version: 1,
                fact_version: snapshot.fact_version,
                fact_rows: snapshot.fact_rows,
                layout_revision: snapshot.layout_revision,
            })
        } else {
            source
                .inner
                .storyline_generation()
                .map(|generation| QuerySnapshot::Storyline {
                    generation: generation.to_string(),
                })
        };
        let local_file_metrics = source.inner.file_metrics().into_iter().collect();
        Ok(Self {
            context,
            backend_info: Some(QueryBackendInfo {
                format,
                tables,
                capabilities,
                source_count,
                snapshot,
            }),
            require_file_join_key: source_count > 1,
            local_file_metrics,
            _catalog_snapshot: None,
        })
    }

    pub(crate) async fn from_catalog_snapshot_with_options(
        snapshot: Arc<DatasetCatalogSnapshot>,
        options: ChronicleQueryExecutionOptions,
    ) -> Result<Self> {
        let context = query_session_context(&options)?;
        snapshot.register(&context).await?;
        let require_file_join_key = snapshot.requires_file_join_key();
        Ok(Self {
            context,
            backend_info: None,
            require_file_join_key,
            local_file_metrics: Vec::new(),
            _catalog_snapshot: Some(snapshot),
        })
    }

    pub fn context(&self) -> &SessionContext {
        &self.context
    }

    pub fn backend_info(&self) -> Option<&QueryBackendInfo> {
        self.backend_info.as_ref()
    }

    pub fn local_file_metrics(&self) -> Option<FileTrajectoryQueryMetricsSnapshot> {
        let mut metrics = self.local_file_metrics.clone();
        if let Some(snapshot) = &self._catalog_snapshot {
            metrics.extend(snapshot.file_metrics());
        }
        let mut snapshots = metrics.iter().map(FileTrajectoryQueryMetrics::snapshot);
        let mut total = snapshots.next()?;
        for snapshot in snapshots {
            total.cache_hits = total.cache_hits.saturating_add(snapshot.cache_hits);
            total.cache_misses = total.cache_misses.saturating_add(snapshot.cache_misses);
            total.cache_evictions = total
                .cache_evictions
                .saturating_add(snapshot.cache_evictions);
            total.files_parsed = total.files_parsed.saturating_add(snapshot.files_parsed);
            total.source_bytes_read = total
                .source_bytes_read
                .saturating_add(snapshot.source_bytes_read);
            total.projected_files = total
                .projected_files
                .saturating_add(snapshot.projected_files);
            total.documents_scanned = total
                .documents_scanned
                .saturating_add(snapshot.documents_scanned);
            total.documents_pruned = total
                .documents_pruned
                .saturating_add(snapshot.documents_pruned);
            total.rows_scanned = total.rows_scanned.saturating_add(snapshot.rows_scanned);
            total.rows_pruned = total.rows_pruned.saturating_add(snapshot.rows_pruned);
            total.rows_emitted = total.rows_emitted.saturating_add(snapshot.rows_emitted);
            total.projected_arrow_bytes = total
                .projected_arrow_bytes
                .saturating_add(snapshot.projected_arrow_bytes);
            total.streamed_records = total
                .streamed_records
                .saturating_add(snapshot.streamed_records);
            total.streaming_buffer_peak_bytes = total
                .streaming_buffer_peak_bytes
                .max(snapshot.streaming_buffer_peak_bytes);
        }
        Some(total)
    }

    /// Register a read-only file source in the same DataFusion context as the
    /// normalized `runs`, `steps`, and `tool_calls` tables.
    pub async fn register_external_table(&self, spec: &ExternalTableSpec) -> Result<()> {
        validate_external_table_spec(spec)?;
        anyhow::ensure!(
            !self
                .context
                .table_exist(spec.name.as_str())
                .map_err(|error| from_datafusion("inspect DataFusion table catalog", error))?,
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
        .map_err(|error| from_datafusion("register external DataFusion table", error))
    }

    /// Build a lazy DataFusion DataFrame for callers that need plan inspection
    /// or further DataFrame transformations.
    pub async fn dataframe(&self, sql: &str) -> Result<DataFrame> {
        ensure_read_only_query(sql)?;
        let dataframe = self
            .context
            .sql(sql)
            .await
            .map_err(|error| from_datafusion("plan pChronicle SQL", error))?;
        if self.require_file_join_key {
            ensure_collision_safe_file_joins(
                dataframe.logical_plan(),
                self._catalog_snapshot.as_deref(),
            )?;
        }
        Ok(dataframe)
    }

    /// Execute SQL and collect Arrow record batches.
    pub async fn query(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        self.dataframe(sql)
            .await?
            .collect()
            .await
            .map_err(|error| from_datafusion("execute pChronicle SQL", error))
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
            .map_err(|error| from_datafusion("start streaming pChronicle SQL", error))?;
        let mut writer = LineDelimitedWriter::new(&mut output);
        let mut rows_written = 0u64;
        while let Some(batch) = stream
            .try_next()
            .await
            .map_err(|error| from_datafusion("stream pChronicle SQL", error))?
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
    let runtime = Arc::new(
        runtime
            .build()
            .map_err(|error| from_datafusion("build DataFusion query runtime", error))?,
    );
    Ok(SessionContext::new_with_config_rt(
        SessionConfig::new().with_information_schema(true),
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
    let statements =
        DFParser::parse_sql(sql).map_err(|error| from_datafusion("parse pChronicle SQL", error))?;
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

fn ensure_collision_safe_file_joins(
    plan: &LogicalPlan,
    catalog: Option<&DatasetCatalogSnapshot>,
) -> Result<()> {
    if let LogicalPlan::Join(join) = plan {
        if join_can_collide_without_file_key(&join.left, &join.right, catalog)
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
        ensure_collision_safe_file_joins(input, catalog)?;
    }
    Ok(())
}

#[derive(Default)]
struct TrajectoryPlanSources {
    legacy: bool,
    datasets: BTreeSet<String>,
}

fn join_can_collide_without_file_key(
    left: &LogicalPlan,
    right: &LogicalPlan,
    catalog: Option<&DatasetCatalogSnapshot>,
) -> bool {
    let left = trajectory_plan_sources(left, catalog);
    let right = trajectory_plan_sources(right, catalog);
    if left.legacy && right.legacy {
        return true;
    }
    let Some(catalog) = catalog else {
        return false;
    };
    left.datasets.intersection(&right.datasets).any(|dataset| {
        catalog
            .dataset(dataset)
            .is_some_and(|dataset| dataset.ready_source_count() > 1)
    })
}

fn trajectory_plan_sources(
    plan: &LogicalPlan,
    catalog: Option<&DatasetCatalogSnapshot>,
) -> TrajectoryPlanSources {
    let mut sources = TrajectoryPlanSources::default();
    collect_trajectory_plan_sources(plan, catalog, &mut sources);
    sources
}

fn collect_trajectory_plan_sources(
    plan: &LogicalPlan,
    catalog: Option<&DatasetCatalogSnapshot>,
    sources: &mut TrajectoryPlanSources,
) {
    if let LogicalPlan::TableScan(scan) = plan {
        if matches!(scan.table_name.table(), "runs" | "steps" | "tool_calls") {
            let dataset = catalog.and_then(|catalog| {
                scan.table_name
                    .schema()
                    .filter(|schema| catalog.dataset(schema).is_some())
                    .or_else(|| {
                        scan.table_name
                            .schema()
                            .is_none_or(|schema| schema == "public")
                            .then(|| catalog.default_dataset())
                            .flatten()
                    })
            });
            if let Some(dataset) = dataset {
                sources.datasets.insert(dataset.to_string());
            } else {
                sources.legacy = true;
            }
        }
    }
    for input in plan.inputs() {
        collect_trajectory_plan_sources(input, catalog, sources);
    }
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
