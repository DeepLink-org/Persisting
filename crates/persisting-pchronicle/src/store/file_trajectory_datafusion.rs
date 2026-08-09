//! Lazy DataFusion datasource for directly querying OpenAI-message and ACTF JSON.
//!
//! Each source file is one streaming partition. Query-only `_file_` predicates
//! are evaluated against the frozen manifest before partitions are opened.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::streaming::StreamingTable;
use datafusion::catalog::Session;
use datafusion::common::ScalarValue;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, Operator, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::stream::RecordBatchReceiverStreamBuilder;
use datafusion::physical_plan::streaming::PartitionStream;
use datafusion::physical_plan::{ExecutionPlan, SendableRecordBatchStream};
use datafusion::prelude::SessionContext;
use lance::deps::arrow_array::{
    ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchOptions, StringArray,
};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, SchemaRef};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use tokio::sync::mpsc::Sender;

use crate::convert::atif_to_storyline;
use crate::{
    actf_to_storylines, parse_openai_msg_corpus_value, split_storyline, ActfDocument,
    ChronicleFormat, StoryRunRow, StoryStepRow, StoryToolCallRow,
};

use super::{
    story_runs_arrow_schema, story_runs_to_batch, story_steps_arrow_schema, story_steps_to_batch,
    story_tool_calls_arrow_schema, story_tool_calls_to_batch, LocalQueryInputFile,
    LocalQueryManifest, StorylineDataFusionTableNames, StorylineTableKind,
};

/// Query-only source path column. This is not part of any Lance table schema.
pub const SOURCE_FILE_COLUMN: &str = "_file_";

pub const DEFAULT_LOCAL_QUERY_BATCH_SIZE: usize = 8192;
pub const DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_LOCAL_QUERY_CACHE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_LOCAL_QUERY_CACHE_FILES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTrajectoryDataSourceOptions {
    pub batch_size: usize,
    /// Hard limit for one source file. The limit is checked before and after reading.
    pub max_file_bytes: u64,
    /// Hard limit for one buffered JSONL/NDJSON record or JSON array element.
    pub max_record_bytes: usize,
    /// Maximum source files parsed concurrently by one datasource.
    pub max_concurrent_files: usize,
    /// Maximum Arrow bytes retained in the shared parsed-file LRU cache.
    pub cache_bytes: usize,
    /// Maximum parsed files retained in the shared LRU cache.
    pub cache_files: usize,
}

impl Default for FileTrajectoryDataSourceOptions {
    fn default() -> Self {
        let concurrency = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(8);
        Self {
            batch_size: DEFAULT_LOCAL_QUERY_BATCH_SIZE,
            max_file_bytes: DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
            max_record_bytes: DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES,
            max_concurrent_files: concurrency,
            cache_bytes: DEFAULT_LOCAL_QUERY_CACHE_BYTES,
            cache_files: DEFAULT_LOCAL_QUERY_CACHE_FILES,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FileTrajectoryQueryMetricsSnapshot {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_evictions: u64,
    pub files_parsed: u64,
    pub source_bytes_read: u64,
    pub projected_files: u64,
    pub documents_scanned: u64,
    pub documents_pruned: u64,
    pub rows_scanned: u64,
    pub rows_pruned: u64,
    pub rows_emitted: u64,
    pub projected_arrow_bytes: u64,
    pub streamed_records: u64,
    pub streaming_buffer_peak_bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct FileTrajectoryQueryMetrics {
    inner: Arc<FileTrajectoryQueryMetricCounters>,
}

impl FileTrajectoryQueryMetrics {
    pub fn snapshot(&self) -> FileTrajectoryQueryMetricsSnapshot {
        FileTrajectoryQueryMetricsSnapshot {
            cache_hits: self.inner.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.inner.cache_misses.load(Ordering::Relaxed),
            cache_evictions: self.inner.cache_evictions.load(Ordering::Relaxed),
            files_parsed: self.inner.files_parsed.load(Ordering::Relaxed),
            source_bytes_read: self.inner.source_bytes_read.load(Ordering::Relaxed),
            projected_files: self.inner.projected_files.load(Ordering::Relaxed),
            documents_scanned: self.inner.documents_scanned.load(Ordering::Relaxed),
            documents_pruned: self.inner.documents_pruned.load(Ordering::Relaxed),
            rows_scanned: self.inner.rows_scanned.load(Ordering::Relaxed),
            rows_pruned: self.inner.rows_pruned.load(Ordering::Relaxed),
            rows_emitted: self.inner.rows_emitted.load(Ordering::Relaxed),
            projected_arrow_bytes: self.inner.projected_arrow_bytes.load(Ordering::Relaxed),
            streamed_records: self.inner.streamed_records.load(Ordering::Relaxed),
            streaming_buffer_peak_bytes: self
                .inner
                .streaming_buffer_peak_bytes
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
struct FileTrajectoryQueryMetricCounters {
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    cache_evictions: AtomicU64,
    files_parsed: AtomicU64,
    source_bytes_read: AtomicU64,
    projected_files: AtomicU64,
    documents_scanned: AtomicU64,
    documents_pruned: AtomicU64,
    rows_scanned: AtomicU64,
    rows_pruned: AtomicU64,
    rows_emitted: AtomicU64,
    projected_arrow_bytes: AtomicU64,
    streamed_records: AtomicU64,
    streaming_buffer_peak_bytes: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTrajectoryFormat {
    Atif,
    OpenaiMsg,
    Actf,
}

impl FileTrajectoryFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Atif => "atif",
            Self::OpenaiMsg => "openai_msg",
            Self::Actf => "actf",
        }
    }

    fn chronicle_format(self) -> ChronicleFormat {
        match self {
            Self::Atif => ChronicleFormat::Atif,
            Self::OpenaiMsg => ChronicleFormat::OpenaiMsg,
            Self::Actf => ChronicleFormat::Actf,
        }
    }

    fn from_chronicle(format: ChronicleFormat) -> Result<Self> {
        match format {
            ChronicleFormat::Atif => Ok(Self::Atif),
            ChronicleFormat::OpenaiMsg => Ok(Self::OpenaiMsg),
            ChronicleFormat::Actf => Ok(Self::Actf),
            _ => anyhow::bail!("file trajectory datasource does not support '{format}'"),
        }
    }
}

#[derive(Debug)]
pub struct FileTrajectoryDataSource {
    format: FileTrajectoryFormat,
    runs: Arc<dyn TableProvider>,
    steps: Arc<dyn TableProvider>,
    tool_calls: Arc<dyn TableProvider>,
    file_count: usize,
    metrics: FileTrajectoryQueryMetrics,
}

pub(crate) type FileTrajectoryProviderParts = (
    Arc<dyn TableProvider>,
    Arc<dyn TableProvider>,
    Arc<dyn TableProvider>,
    usize,
    FileTrajectoryQueryMetrics,
);

impl FileTrajectoryDataSource {
    pub fn open_openai_msg(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::open(path, FileTrajectoryFormat::OpenaiMsg)
    }

    pub fn open_actf(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::open(path, FileTrajectoryFormat::Actf)
    }

    pub fn open_atif(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::open(path, FileTrajectoryFormat::Atif)
    }

    pub fn open(path: impl AsRef<std::path::Path>, format: FileTrajectoryFormat) -> Result<Self> {
        let manifest = LocalQueryManifest::for_format(path, format.chronicle_format())?;
        Self::from_manifest(manifest)
    }

    pub fn from_manifest(manifest: LocalQueryManifest) -> Result<Self> {
        Self::from_manifest_with_options(manifest, FileTrajectoryDataSourceOptions::default())
    }

    pub fn from_manifest_with_options(
        manifest: LocalQueryManifest,
        options: FileTrajectoryDataSourceOptions,
    ) -> Result<Self> {
        validate_options(options)?;
        let format = FileTrajectoryFormat::from_chronicle(manifest.format())?;
        let manifest = Arc::new(manifest);
        let file_count = manifest.file_count();
        let files = manifest
            .files()
            .iter()
            .cloned()
            .map(FileState::new)
            .map(Arc::new)
            .collect::<Arc<[_]>>();
        let runtime = Arc::new(FileTrajectoryRuntime::new(options));
        let metrics = runtime.metrics.clone();
        Ok(Self {
            format,
            runs: Arc::new(FileTrajectoryTableProvider::new(
                files.clone(),
                runtime.clone(),
                format,
                StorylineTableKind::Runs,
            )),
            steps: Arc::new(FileTrajectoryTableProvider::new(
                files.clone(),
                runtime.clone(),
                format,
                StorylineTableKind::Steps,
            )),
            tool_calls: Arc::new(FileTrajectoryTableProvider::new(
                files,
                runtime,
                format,
                StorylineTableKind::ToolCalls,
            )),
            file_count,
            metrics,
        })
    }

    pub(crate) fn into_providers(self) -> FileTrajectoryProviderParts {
        (
            self.runs,
            self.steps,
            self.tool_calls,
            self.file_count,
            self.metrics,
        )
    }

    pub fn format(&self) -> FileTrajectoryFormat {
        self.format
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }

    pub fn metrics(&self) -> FileTrajectoryQueryMetrics {
        self.metrics.clone()
    }

    pub fn register(&self, context: &SessionContext) -> Result<()> {
        let names = StorylineDataFusionTableNames::default();
        context
            .register_table(&names.runs, self.runs.clone())
            .context("register file-backed runs query table")?;
        context
            .register_table(&names.steps, self.steps.clone())
            .context("register file-backed steps query table")?;
        context
            .register_table(&names.tool_calls, self.tool_calls.clone())
            .context("register file-backed tool_calls query table")?;
        Ok(())
    }

    pub fn session_context(&self) -> Result<SessionContext> {
        let context = SessionContext::new();
        self.register(&context)?;
        Ok(context)
    }
}

#[derive(Debug, Clone)]
struct FileScanSpec {
    projection: Option<Arc<[usize]>>,
    projected_names: Arc<HashSet<String>>,
    step_filters: Arc<[AtifStepFilter]>,
}

impl FileScanSpec {
    fn new(projection: Option<&Vec<usize>>, filters: &[Expr], schema: &SchemaRef) -> Self {
        let step_filters = filters
            .iter()
            .filter_map(atif_step_filters)
            .flatten()
            .collect::<Arc<[_]>>();
        Self {
            projection: projection.map(|values| Arc::from(values.clone())),
            projected_names: Arc::new(match projection {
                Some(projection) => projection
                    .iter()
                    .map(|index| schema.field(*index).name().clone())
                    .collect(),
                None => schema
                    .fields()
                    .iter()
                    .map(|field| field.name().clone())
                    .collect(),
            }),
            step_filters,
        }
    }

    fn can_project_atif_steps(&self, schema: &SchemaRef) -> bool {
        self.projection
            .as_ref()
            .is_some_and(|projection| projection.len() < schema.fields().len())
    }

    fn wants(&self, name: &str) -> bool {
        self.projected_names.contains(name)
    }

    fn matches_document(&self, session_id: &str) -> bool {
        self.step_filters
            .iter()
            .all(|filter| filter.matches_document(session_id))
    }

    fn matches_step(&self, step_id: i64, source: &str) -> bool {
        self.step_filters
            .iter()
            .all(|filter| filter.matches_step(step_id, source))
    }
}

#[derive(Debug, Clone)]
enum AtifStepFilter {
    SessionId(StringPredicate),
    Source(StringPredicate),
    StepId(IntPredicate),
}

impl AtifStepFilter {
    fn matches_document(&self, session_id: &str) -> bool {
        match self {
            Self::SessionId(predicate) => predicate.matches(session_id),
            Self::Source(_) | Self::StepId(_) => true,
        }
    }

    fn matches_step(&self, step_id: i64, source: &str) -> bool {
        match self {
            Self::SessionId(_) => true,
            Self::Source(predicate) => predicate.matches(source),
            Self::StepId(predicate) => predicate.matches(step_id),
        }
    }
}

#[derive(Debug, Clone)]
enum StringPredicate {
    Equal(String),
    NotEqual(String),
    In { values: Vec<String>, negated: bool },
}

impl StringPredicate {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Equal(expected) => value == expected,
            Self::NotEqual(expected) => value != expected,
            Self::In { values, negated } => values.iter().any(|item| item == value) != *negated,
        }
    }
}

#[derive(Debug, Clone)]
enum IntPredicate {
    Compare { op: Operator, value: i64 },
    Between { low: i64, high: i64, negated: bool },
    In { values: Vec<i64>, negated: bool },
}

impl IntPredicate {
    fn matches(&self, candidate: i64) -> bool {
        match self {
            Self::Compare { op, value } => match op {
                Operator::Eq => candidate == *value,
                Operator::NotEq => candidate != *value,
                Operator::Lt => candidate < *value,
                Operator::LtEq => candidate <= *value,
                Operator::Gt => candidate > *value,
                Operator::GtEq => candidate >= *value,
                _ => true,
            },
            Self::Between { low, high, negated } => {
                (candidate >= *low && candidate <= *high) != *negated
            }
            Self::In { values, negated } => values.contains(&candidate) != *negated,
        }
    }
}

#[derive(Debug)]
struct FileTrajectoryTableProvider {
    files: Arc<[Arc<FileState>]>,
    runtime: Arc<FileTrajectoryRuntime>,
    format: FileTrajectoryFormat,
    kind: StorylineTableKind,
    schema: SchemaRef,
    batch_size: usize,
}

impl FileTrajectoryTableProvider {
    fn new(
        files: Arc<[Arc<FileState>]>,
        runtime: Arc<FileTrajectoryRuntime>,
        format: FileTrajectoryFormat,
        kind: StorylineTableKind,
    ) -> Self {
        let batch_size = runtime.options.batch_size;
        Self {
            files,
            runtime,
            format,
            kind,
            schema: query_schema(&base_schema(kind)),
            batch_size,
        }
    }

    fn selected_files(&self, filters: &[Expr]) -> Vec<Arc<FileState>> {
        self.files
            .iter()
            .filter(|file| {
                filters.iter().all(|filter| {
                    matches_file_filter(filter, file.file.relative_path()).unwrap_or(true)
                })
            })
            .cloned()
            .collect()
    }
}

#[async_trait]
impl TableProvider for FileTrajectoryTableProvider {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
        let scan = Arc::new(FileScanSpec::new(projection, filters, &self.schema));
        let output_schema = projected_schema(&self.schema, projection)?;
        let partitions = self
            .selected_files(filters)
            .into_iter()
            .map(|file| {
                Arc::new(FileTrajectoryPartition {
                    file,
                    runtime: self.runtime.clone(),
                    format: self.format,
                    kind: self.kind,
                    source_schema: self.schema.clone(),
                    schema: output_schema.clone(),
                    batch_size: self.batch_size,
                    scan: scan.clone(),
                }) as Arc<dyn PartitionStream>
            })
            .collect::<Vec<_>>();
        let table = StreamingTable::try_new(output_schema, partitions)?;
        table.scan(state, None, &[], limit).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|filter| {
                if matches_file_filter(filter, "").is_some() {
                    TableProviderFilterPushDown::Exact
                } else if self.format == FileTrajectoryFormat::Atif
                    && self.kind == StorylineTableKind::Steps
                    && atif_step_filters(filter).is_some()
                {
                    // The projected decoder applies these filters to reduce
                    // materialization, while DataFusion retains the filter to
                    // guarantee SQL semantics on the compatibility fallback.
                    TableProviderFilterPushDown::Inexact
                } else {
                    TableProviderFilterPushDown::Unsupported
                }
            })
            .collect())
    }
}

#[derive(Debug)]
struct FileTrajectoryPartition {
    file: Arc<FileState>,
    runtime: Arc<FileTrajectoryRuntime>,
    format: FileTrajectoryFormat,
    kind: StorylineTableKind,
    source_schema: SchemaRef,
    schema: SchemaRef,
    batch_size: usize,
    scan: Arc<FileScanSpec>,
}

impl PartitionStream for FileTrajectoryPartition {
    fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    fn execute(&self, _ctx: Arc<datafusion::execution::TaskContext>) -> SendableRecordBatchStream {
        let mut builder = RecordBatchReceiverStreamBuilder::new(self.schema.clone(), 2);
        let tx = builder.tx();
        let file = self.file.clone();
        let runtime = self.runtime.clone();
        let format = self.format;
        let kind = self.kind;
        let source_schema = self.source_schema.clone();
        let schema = self.schema.clone();
        let batch_size = self.batch_size;
        let scan = self.scan.clone();
        builder.spawn_blocking(move || {
            stream_file(
                &file,
                &runtime,
                format,
                kind,
                source_schema,
                schema,
                batch_size,
                &scan,
                &tx,
            )
            .map_err(datafusion_error)
        });
        builder.build()
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_file(
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    format: FileTrajectoryFormat,
    kind: StorylineTableKind,
    source_schema: SchemaRef,
    schema: SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<()> {
    if format == FileTrajectoryFormat::Atif
        && kind == StorylineTableKind::Steps
        && scan.can_project_atif_steps(&source_schema)
    {
        stream_projected_atif_steps(file, runtime, &schema, batch_size, scan, tx)?;
        return Ok(());
    }
    let parsed = load_file(file, runtime, format)?;
    for batch in parsed.batches(kind) {
        let batch = match &scan.projection {
            Some(projection) => batch
                .project(projection)
                .context("project fallback JSON batch")?,
            None => batch.clone(),
        };
        if tx.blocking_send(Ok(batch)).is_err() {
            break;
        }
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct ProjectedAtifAgent {
    name: String,
    version: String,
}

#[derive(Debug)]
struct ProjectedAtifTrajectory {
    schema_version: String,
    session_id: Option<String>,
    trajectory_id: Option<String>,
    agent: ProjectedAtifAgent,
    steps: Vec<ProjectedAtifStep>,
    skipped_steps: usize,
}

impl ProjectedAtifTrajectory {
    fn effective_session_id(&self) -> Result<&str> {
        self.session_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                self.trajectory_id
                    .as_deref()
                    .filter(|value| !value.is_empty())
            })
            .context("ATIF trajectory requires session_id or trajectory_id")
    }

    fn step_count(&self) -> usize {
        self.steps.len() + self.skipped_steps
    }
}

#[derive(Debug)]
struct ProjectedAtifStep {
    step_id: i64,
    timestamp: Option<String>,
    source: String,
    model_name: Option<String>,
    reasoning_effort: Option<serde_json::Value>,
    message: serde_json::Value,
    reasoning_content: Option<String>,
    tool_calls_nonempty: bool,
    observation_present: bool,
    metrics: Option<serde_json::Value>,
    extra: Option<serde_json::Value>,
    llm_call_count: Option<i64>,
    is_copied_context: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedAtifTrajectoryField {
    SchemaVersion,
    SessionId,
    TrajectoryId,
    Agent,
    Steps,
    #[serde(other)]
    Other,
}

impl ProjectedAtifTrajectoryField {
    fn name(self) -> &'static str {
        match self {
            Self::SchemaVersion => "schema_version",
            Self::SessionId => "session_id",
            Self::TrajectoryId => "trajectory_id",
            Self::Agent => "agent",
            Self::Steps => "steps",
            Self::Other => "<unknown>",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum ProjectedAtifStepField {
    StepId,
    Timestamp,
    Source,
    ModelName,
    ReasoningEffort,
    Message,
    ReasoningContent,
    ToolCalls,
    Observation,
    Metrics,
    Extra,
    LlmCallCount,
    IsCopiedContext,
    #[serde(other)]
    Other,
}

impl ProjectedAtifStepField {
    fn name(self) -> &'static str {
        match self {
            Self::StepId => "step_id",
            Self::Timestamp => "timestamp",
            Self::Source => "source",
            Self::ModelName => "model_name",
            Self::ReasoningEffort => "reasoning_effort",
            Self::Message => "message",
            Self::ReasoningContent => "reasoning_content",
            Self::ToolCalls => "tool_calls",
            Self::Observation => "observation",
            Self::Metrics => "metrics",
            Self::Extra => "extra",
            Self::LlmCallCount => "llm_call_count",
            Self::IsCopiedContext => "is_copied_context",
            Self::Other => "<unknown>",
        }
    }
}

struct ProjectedAtifTrajectorySeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifTrajectorySeed<'_> {
    type Value = ProjectedAtifTrajectory;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedAtifTrajectoryVisitor { scan: self.scan })
    }
}

struct ProjectedAtifTrajectoryVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifTrajectoryVisitor<'_> {
    type Value = ProjectedAtifTrajectory;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF trajectory object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::new();
        let mut schema_version = None;
        let mut session_id = None;
        let mut trajectory_id = None;
        let mut agent = None;
        let mut steps = None;
        let mut skipped_steps = 0;

        while let Some(field) = map.next_key::<ProjectedAtifTrajectoryField>()? {
            if field != ProjectedAtifTrajectoryField::Other && !seen.insert(field) {
                return Err(de::Error::duplicate_field(field.name()));
            }
            match field {
                ProjectedAtifTrajectoryField::SchemaVersion => {
                    schema_version = Some(map.next_value::<String>()?);
                }
                ProjectedAtifTrajectoryField::SessionId => {
                    session_id = map.next_value::<Option<String>>()?;
                }
                ProjectedAtifTrajectoryField::TrajectoryId => {
                    trajectory_id = map.next_value::<Option<String>>()?;
                }
                ProjectedAtifTrajectoryField::Agent => {
                    agent = Some(map.next_value::<ProjectedAtifAgent>()?);
                }
                ProjectedAtifTrajectoryField::Steps => {
                    let known_session = session_id
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .or_else(|| trajectory_id.as_deref().filter(|value| !value.is_empty()));
                    if known_session.is_some_and(|value| !self.scan.matches_document(value)) {
                        skipped_steps = map.next_value_seed(CountSequenceSeed)?;
                        steps = Some(Vec::new());
                    } else {
                        steps =
                            Some(map.next_value_seed(ProjectedAtifStepsSeed { scan: self.scan })?);
                    }
                }
                ProjectedAtifTrajectoryField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        Ok(ProjectedAtifTrajectory {
            schema_version: schema_version
                .ok_or_else(|| de::Error::missing_field("schema_version"))?,
            session_id,
            trajectory_id,
            agent: agent.ok_or_else(|| de::Error::missing_field("agent"))?,
            steps: steps.ok_or_else(|| de::Error::missing_field("steps"))?,
            skipped_steps,
        })
    }
}

struct ProjectedAtifStepsSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepsSeed<'_> {
    type Value = Vec<ProjectedAtifStep>;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ProjectedAtifStepsVisitor { scan: self.scan })
    }
}

struct ProjectedAtifStepsVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifStepsVisitor<'_> {
    type Value = Vec<ProjectedAtifStep>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF steps array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut steps = Vec::with_capacity(sequence.size_hint().unwrap_or_default().min(8192));
        while let Some(step) =
            sequence.next_element_seed(ProjectedAtifStepSeed { scan: self.scan })?
        {
            steps.push(step);
        }
        Ok(steps)
    }
}

struct ProjectedAtifStepSeed<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> DeserializeSeed<'de> for ProjectedAtifStepSeed<'_> {
    type Value = ProjectedAtifStep;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ProjectedAtifStepVisitor { scan: self.scan })
    }
}

struct ProjectedAtifStepVisitor<'a> {
    scan: &'a FileScanSpec,
}

impl<'de> Visitor<'de> for ProjectedAtifStepVisitor<'_> {
    type Value = ProjectedAtifStep;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an ATIF step object")
    }

    fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut seen = HashSet::new();
        let mut step_id = None;
        let mut timestamp = None;
        let mut source = None;
        let mut model_name = None;
        let mut reasoning_effort = None;
        let mut message = serde_json::Value::Null;
        let mut message_seen = false;
        let mut reasoning_content = None;
        let mut tool_calls_nonempty = false;
        let mut observation_present = false;
        let mut metrics = None;
        let mut extra = None;
        let mut llm_call_count = None;
        let mut is_copied_context = None;

        while let Some(field) = map.next_key::<ProjectedAtifStepField>()? {
            if field != ProjectedAtifStepField::Other && !seen.insert(field) {
                return Err(de::Error::duplicate_field(field.name()));
            }
            match field {
                ProjectedAtifStepField::StepId => step_id = Some(map.next_value::<i64>()?),
                ProjectedAtifStepField::Timestamp => {
                    if self.scan.wants("timestamp") {
                        timestamp = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Source => source = Some(map.next_value::<String>()?),
                ProjectedAtifStepField::ModelName => {
                    if self.scan.wants("model_name") {
                        model_name = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningEffort => {
                    if self.scan.wants("reasoning_effort_json") {
                        reasoning_effort = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Message => {
                    message_seen = true;
                    if self.scan.wants("message_json") {
                        message = map.next_value::<serde_json::Value>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ReasoningContent => {
                    if self.scan.wants("reasoning_content") {
                        reasoning_content = map.next_value::<Option<String>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::ToolCalls => {
                    if self.scan.wants("kind") || self.scan.wants("effective_kind") {
                        tool_calls_nonempty = map
                            .next_value::<Option<Vec<IgnoredAny>>>()?
                            .is_some_and(|calls| !calls.is_empty());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Observation => {
                    if self.scan.wants("had_observation") {
                        observation_present = map.next_value::<Option<IgnoredAny>>()?.is_some();
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Metrics => {
                    if self.scan.wants("metrics_json")
                        || self.scan.wants("latency_ms")
                        || self.scan.wants("ttft_ms")
                    {
                        metrics = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Extra => {
                    if self.scan.wants("extra_json") {
                        extra = map.next_value::<Option<serde_json::Value>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::LlmCallCount => {
                    if self.scan.wants("llm_call_count") {
                        llm_call_count = map.next_value::<Option<i64>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::IsCopiedContext => {
                    if self.scan.wants("is_copied_context") {
                        is_copied_context = map.next_value::<Option<bool>>()?;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                ProjectedAtifStepField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !message_seen {
            return Err(de::Error::missing_field("message"));
        }
        Ok(ProjectedAtifStep {
            step_id: step_id.ok_or_else(|| de::Error::missing_field("step_id"))?,
            timestamp,
            source: source.ok_or_else(|| de::Error::missing_field("source"))?,
            model_name,
            reasoning_effort,
            message,
            reasoning_content,
            tool_calls_nonempty,
            observation_present,
            metrics,
            extra,
            llm_call_count,
            is_copied_context,
        })
    }
}

struct CountSequenceSeed;

impl<'de> DeserializeSeed<'de> for CountSequenceSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(CountSequenceVisitor)
    }
}

struct CountSequenceVisitor;

impl<'de> Visitor<'de> for CountSequenceVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0;
        while sequence.next_element::<IgnoredAny>()?.is_some() {
            count += 1;
        }
        Ok(count)
    }
}

const PROJECTED_QUERY_CANCELLED: &str = "pChronicle projected query receiver closed";

struct ProjectedAtifStream<'a> {
    file: &'a Arc<FileState>,
    runtime: &'a Arc<FileTrajectoryRuntime>,
    schema: &'a SchemaRef,
    batch_size: usize,
    scan: &'a FileScanSpec,
    tx: &'a Sender<datafusion::common::Result<RecordBatch>>,
    pending: Vec<StoryStepRow>,
    session_ids: HashSet<String>,
    cancelled: bool,
}

impl<'a> ProjectedAtifStream<'a> {
    fn new(
        file: &'a Arc<FileState>,
        runtime: &'a Arc<FileTrajectoryRuntime>,
        schema: &'a SchemaRef,
        batch_size: usize,
        scan: &'a FileScanSpec,
        tx: &'a Sender<datafusion::common::Result<RecordBatch>>,
    ) -> Self {
        Self {
            file,
            runtime,
            schema,
            batch_size,
            scan,
            tx,
            pending: Vec::with_capacity(batch_size),
            session_ids: HashSet::new(),
            cancelled: false,
        }
    }

    fn consume(&mut self, trajectory: ProjectedAtifTrajectory) -> Result<()> {
        self.runtime
            .metrics
            .inner
            .streamed_records
            .fetch_add(1, Ordering::Relaxed);
        if !project_atif_trajectory(
            trajectory,
            self.file,
            self.runtime,
            self.schema,
            self.batch_size,
            self.scan,
            self.tx,
            &mut self.pending,
            &mut self.session_ids,
        )? {
            self.cancelled = true;
            anyhow::bail!(PROJECTED_QUERY_CANCELLED);
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if !self.cancelled {
            let _ = emit_projected_step_batch(
                &mut self.pending,
                self.file,
                self.runtime,
                self.schema,
                self.tx,
            )?;
        }
        Ok(())
    }
}

struct BoundedCountingReader<R> {
    inner: R,
    bytes_read: u64,
    maximum: u64,
}

impl<R> BoundedCountingReader<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            bytes_read: 0,
            maximum,
        }
    }

    fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

impl<R: Read> Read for BoundedCountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.bytes_read);
        if remaining == 0 {
            let mut probe = [0_u8; 1];
            if self.inner.read(&mut probe)? == 0 {
                return Ok(0);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("trajectory input exceeded {} bytes", self.maximum),
            ));
        }
        let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = self.inner.read(&mut buffer[..maximum])?;
        self.bytes_read += read as u64;
        Ok(read)
    }
}

fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(buffer.len());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if buffer.len().saturating_add(end) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSONL record exceeded max_record_bytes {maximum}"),
            ));
        }
        buffer.extend_from_slice(&available[..end]);
        let ended = available[end - 1] == b'\n';
        reader.consume(end);
        if ended {
            return Ok(buffer.len());
        }
    }
}

/// Copy one complete top-level JSON object out of a buffered stream.
///
/// This scanner only discovers the record boundary; serde remains the source
/// of truth for JSON syntax and ATIF validation. Strings and escapes are
/// tracked so braces inside message text do not terminate the record.
fn read_bounded_json_object<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<usize> {
    buffer.clear();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "unterminated JSON object in array",
            ));
        }
        let mut end = available.len();
        let mut finished = false;
        for (index, byte) in available.iter().copied().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth = depth.checked_add(1).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "JSON nesting depth overflow")
                    })?;
                }
                b'}' | b']' => {
                    if depth == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unexpected JSON closing delimiter",
                        ));
                    }
                    depth -= 1;
                    if depth == 0 {
                        if byte != b'}' {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "ATIF array element must be a JSON object",
                            ));
                        }
                        end = index + 1;
                        finished = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        if buffer.len().saturating_add(end) > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("JSON array record exceeded max_record_bytes {maximum}"),
            ));
        }
        buffer.extend_from_slice(&available[..end]);
        reader.consume(end);
        if finished {
            return Ok(buffer.len());
        }
    }
}

fn trim_ascii_whitespace(mut input: &[u8]) -> &[u8] {
    while input.first().is_some_and(u8::is_ascii_whitespace) {
        input = &input[1..];
    }
    while input.last().is_some_and(u8::is_ascii_whitespace) {
        input = &input[..input.len() - 1];
    }
    input
}

fn first_non_whitespace<R: BufRead>(reader: &mut R) -> io::Result<Option<u8>> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(None);
        }
        if let Some(index) = available
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
        {
            let first = available[index];
            reader.consume(index);
            return Ok(Some(first));
        }
        let length = available.len();
        reader.consume(length);
    }
}

fn is_ndjson(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "jsonl" | "ndjson"))
}

fn stream_projected_atif_array<R: BufRead>(
    reader: &mut R,
    reader_capacity: usize,
    stream: &mut ProjectedAtifStream<'_>,
    maximum_record_bytes: usize,
) -> Result<()> {
    anyhow::ensure!(
        first_non_whitespace(reader)? == Some(b'['),
        "projected ATIF array must start with '['"
    );
    reader.consume(1);

    let mut first = true;
    let mut ordinal = 0_usize;
    let mut record = Vec::new();
    loop {
        if !first {
            match first_non_whitespace(reader)? {
                Some(b']') => {
                    reader.consume(1);
                    anyhow::ensure!(
                        first_non_whitespace(reader)?.is_none(),
                        "trailing content after ATIF JSON array"
                    );
                    return Ok(());
                }
                Some(b',') => reader.consume(1),
                Some(other) => {
                    anyhow::bail!("ATIF JSON array expected ',' or ']', found byte 0x{other:02x}")
                }
                None => anyhow::bail!("unterminated ATIF JSON array"),
            }
        }

        match first_non_whitespace(reader)? {
            Some(b']') if first => anyhow::bail!("ATIF input contains no trajectories"),
            Some(b'{') => {}
            Some(other) => {
                anyhow::bail!("ATIF JSON array element must be an object, found byte 0x{other:02x}")
            }
            None => anyhow::bail!("unterminated ATIF JSON array"),
        }

        ordinal += 1;
        read_bounded_json_object(reader, &mut record, maximum_record_bytes)
            .with_context(|| format!("read projected ATIF array element {ordinal}"))?;
        stream
            .runtime
            .metrics
            .inner
            .streaming_buffer_peak_bytes
            .fetch_max(
                reader_capacity.saturating_add(record.capacity()) as u64,
                Ordering::Relaxed,
            );
        let mut deserializer = serde_json::Deserializer::from_slice(&record);
        let trajectory = ProjectedAtifTrajectorySeed { scan: stream.scan }
            .deserialize(&mut deserializer)
            .with_context(|| format!("parse projected ATIF array element {ordinal}"))?;
        deserializer
            .end()
            .with_context(|| format!("finish projected ATIF array element {ordinal}"))?;
        stream.consume(trajectory)?;
        first = false;
    }
}

fn stream_projected_atif_steps(
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<()> {
    let _permit = runtime.limiter.acquire()?;
    file.file.validate_unchanged()?;
    anyhow::ensure!(
        file.file.size_bytes() <= runtime.options.max_file_bytes,
        "ATIF input {} is {} bytes, exceeding max_file_bytes {}",
        file.file.path().display(),
        file.file.size_bytes(),
        runtime.options.max_file_bytes
    );
    let input = File::open(file.file.path())
        .with_context(|| format!("open ATIF input {}", file.file.path().display()))?;
    let mut reader = BufReader::with_capacity(
        64 * 1024,
        BoundedCountingReader::new(input, runtime.options.max_file_bytes),
    );
    runtime
        .metrics
        .inner
        .streaming_buffer_peak_bytes
        .fetch_max(reader.capacity() as u64, Ordering::Relaxed);
    let mut stream = ProjectedAtifStream::new(file, runtime, schema, batch_size, scan, tx);

    if is_ndjson(file.file.path()) {
        let mut record = Vec::new();
        let mut line_number = 0_usize;
        let mut parsed_records = 0_usize;
        loop {
            let read =
                read_bounded_line(&mut reader, &mut record, runtime.options.max_record_bytes)
                    .with_context(|| {
                        format!("read projected ATIF JSONL {}", file.file.path().display())
                    })?;
            if read == 0 {
                break;
            }
            line_number += 1;
            runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                reader.capacity().saturating_add(record.capacity()) as u64,
                Ordering::Relaxed,
            );
            let record = trim_ascii_whitespace(&record);
            if record.is_empty() {
                continue;
            }
            let mut deserializer = serde_json::Deserializer::from_slice(record);
            let trajectory = ProjectedAtifTrajectorySeed { scan }
                .deserialize(&mut deserializer)
                .with_context(|| {
                    format!(
                        "parse projected ATIF JSONL {} line {line_number}",
                        file.file.path().display()
                    )
                })?;
            deserializer.end().with_context(|| {
                format!(
                    "finish projected ATIF JSONL {} line {line_number}",
                    file.file.path().display()
                )
            })?;
            if let Err(error) = stream.consume(trajectory) {
                if stream.cancelled {
                    break;
                }
                return Err(error);
            }
            parsed_records += 1;
        }
        anyhow::ensure!(
            parsed_records > 0 || stream.cancelled,
            "ATIF input contains no trajectories: {}",
            file.file.path().display()
        );
    } else {
        let shape = first_non_whitespace(&mut reader)
            .with_context(|| format!("inspect ATIF input {}", file.file.path().display()))?
            .with_context(|| format!("ATIF input is empty: {}", file.file.path().display()))?;
        let result = match shape {
            b'{' => {
                let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
                let result = ProjectedAtifTrajectorySeed { scan }
                    .deserialize(&mut deserializer)
                    .map_err(anyhow::Error::from)
                    .and_then(|trajectory| stream.consume(trajectory));
                match result {
                    Ok(()) => deserializer.end().map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                }
            }
            b'[' => {
                let reader_capacity = reader.capacity();
                stream_projected_atif_array(
                    &mut reader,
                    reader_capacity,
                    &mut stream,
                    runtime.options.max_record_bytes,
                )
            }
            _ => anyhow::bail!(
                "ATIF input {} must contain an object, array, JSONL, or NDJSON",
                file.file.path().display()
            ),
        };
        if let Err(error) = result {
            if !stream.cancelled {
                return Err(error).with_context(|| {
                    format!("parse projected ATIF input {}", file.file.path().display())
                });
            }
        }
    }
    stream.finish()?;
    let bytes_read = reader.get_ref().bytes_read();
    runtime
        .metrics
        .inner
        .source_bytes_read
        .fetch_add(bytes_read, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .files_parsed
        .fetch_add(1, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .projected_files
        .fetch_add(1, Ordering::Relaxed);
    if !stream.cancelled {
        file.file.validate_unchanged()?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn project_atif_trajectory(
    trajectory: ProjectedAtifTrajectory,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
    pending: &mut Vec<StoryStepRow>,
    session_ids: &mut HashSet<String>,
) -> Result<bool> {
    let session_id = trajectory.effective_session_id()?.to_string();
    let run_id = trajectory
        .trajectory_id
        .as_deref()
        .unwrap_or(&session_id)
        .to_string();
    anyhow::ensure!(
        !trajectory.agent.name.is_empty(),
        "ATIF agent.name is required"
    );
    anyhow::ensure!(
        !trajectory.agent.version.is_empty(),
        "ATIF agent.version is required"
    );
    let _ = &trajectory.schema_version;
    anyhow::ensure!(
        session_ids.insert(session_id.clone()),
        "duplicate ATIF session_id '{}' in {}",
        session_id,
        file.file.path().display()
    );
    runtime
        .metrics
        .inner
        .documents_scanned
        .fetch_add(1, Ordering::Relaxed);
    runtime
        .metrics
        .inner
        .rows_scanned
        .fetch_add(trajectory.step_count() as u64, Ordering::Relaxed);
    if !scan.matches_document(&session_id) {
        runtime
            .metrics
            .inner
            .documents_pruned
            .fetch_add(1, Ordering::Relaxed);
        runtime
            .metrics
            .inner
            .rows_pruned
            .fetch_add(trajectory.step_count() as u64, Ordering::Relaxed);
        return Ok(true);
    }

    let mut rows = Vec::with_capacity(trajectory.steps.len());
    let mut step_ids = HashSet::with_capacity(trajectory.steps.len());
    for step in trajectory.steps {
        anyhow::ensure!(step.step_id >= 1, "ATIF step_id must start from 1");
        anyhow::ensure!(
            step_ids.insert(step.step_id),
            "duplicate ATIF step_id {} in session {}",
            step.step_id,
            session_id
        );
        if !scan.matches_step(step.step_id, &step.source) {
            runtime
                .metrics
                .inner
                .rows_pruned
                .fetch_add(1, Ordering::Relaxed);
            continue;
        }
        rows.push(project_atif_step(&run_id, &session_id, step, scan));
    }
    rows.sort_by_key(|row| row.step_id);
    runtime
        .metrics
        .inner
        .rows_emitted
        .fetch_add(rows.len() as u64, Ordering::Relaxed);
    for row in rows {
        pending.push(row);
        if pending.len() == batch_size
            && !emit_projected_step_batch(pending, file, runtime, schema, tx)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn project_atif_step(
    run_id: &str,
    session_id: &str,
    step: ProjectedAtifStep,
    scan: &FileScanSpec,
) -> StoryStepRow {
    let wants_kind = scan.wants("kind");
    let wants_effective_kind = scan.wants("effective_kind");
    let effective_kind = match step.source.as_str() {
        "user" => "dialogue",
        "system" => "internal",
        "agent" if step.tool_calls_nonempty => "autonomous",
        _ => "dialogue",
    };
    let kind = if matches!(
        (step.source.as_str(), effective_kind),
        ("user", "dialogue") | ("system", "internal") | ("agent", "dialogue")
    ) {
        None
    } else {
        Some(effective_kind.to_string())
    };

    let (latency_ms, ttft_ms) = projected_timing_from_metrics(step.metrics.as_ref());
    StoryStepRow {
        run_id: if scan.wants("run_id") {
            run_id.to_string()
        } else {
            String::new()
        },
        session_id: if scan.wants("session_id") {
            session_id.to_string()
        } else {
            String::new()
        },
        step_id: step.step_id,
        kind: wants_kind.then_some(kind).flatten(),
        effective_kind: if wants_effective_kind {
            effective_kind.to_string()
        } else {
            String::new()
        },
        timestamp: scan.wants("timestamp").then_some(step.timestamp).flatten(),
        source: if scan.wants("source") {
            step.source
        } else {
            String::new()
        },
        message: if scan.wants("message_json") {
            step.message
        } else {
            serde_json::Value::Null
        },
        reasoning_content: scan
            .wants("reasoning_content")
            .then_some(step.reasoning_content)
            .flatten(),
        reasoning_effort: scan
            .wants("reasoning_effort_json")
            .then_some(step.reasoning_effort)
            .flatten(),
        metrics: scan.wants("metrics_json").then_some(step.metrics).flatten(),
        model_name: scan
            .wants("model_name")
            .then_some(step.model_name)
            .flatten(),
        llm_call_count: scan
            .wants("llm_call_count")
            .then_some(step.llm_call_count)
            .flatten(),
        is_copied_context: scan
            .wants("is_copied_context")
            .then_some(step.is_copied_context)
            .flatten(),
        latency_ms: scan.wants("latency_ms").then_some(latency_ms).flatten(),
        ttft_ms: scan.wants("ttft_ms").then_some(ttft_ms).flatten(),
        had_observation: scan.wants("had_observation") && step.observation_present,
        extra: scan.wants("extra_json").then_some(step.extra).flatten(),
    }
}

fn projected_timing_from_metrics(
    metrics: Option<&serde_json::Value>,
) -> (Option<i64>, Option<i64>) {
    let Some(metrics) = metrics else {
        return (None, None);
    };
    let latency_ms = metrics
        .get("latency_ms")
        .or_else(|| metrics.get("elapsed_ms"))
        .or_else(|| metrics.get("duration_ms"))
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_f64().map(|value| value as i64))
        });
    let ttft_ms = metrics.get("ttft_ms").and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_f64().map(|value| value as i64))
    });
    (latency_ms, ttft_ms)
}

fn emit_projected_step_batch(
    rows: &mut Vec<StoryStepRow>,
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    schema: &SchemaRef,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<bool> {
    if rows.is_empty() {
        return Ok(true);
    }
    let batch = projected_step_rows_to_batch(rows, file.file.relative_path(), schema.clone())?;
    rows.clear();
    runtime
        .metrics
        .inner
        .projected_arrow_bytes
        .fetch_add(batch.get_array_memory_size() as u64, Ordering::Relaxed);
    Ok(tx.blocking_send(Ok(batch)).is_ok())
}

fn projected_step_rows_to_batch(
    rows: &[StoryStepRow],
    relative_path: &str,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let mut columns = Vec::<ArrayRef>::with_capacity(schema.fields().len());
    for field in schema.fields() {
        let column: ArrayRef = match field.name().as_str() {
            "run_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.run_id.as_str()),
            )),
            "session_id" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.session_id.as_str()),
            )),
            "step_id" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.step_id).collect::<Vec<_>>(),
            )),
            "kind" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.kind.as_deref()),
            )),
            "effective_kind" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.effective_kind.as_str()),
            )),
            "timestamp" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.timestamp.as_deref()),
            )),
            "source" => Arc::new(StringArray::from_iter_values(
                rows.iter().map(|row| row.source.as_str()),
            )),
            "message_json" => Arc::new(StringArray::from_iter_values(
                rows.iter()
                    .map(|row| serde_json::to_string(&row.message))
                    .collect::<serde_json::Result<Vec<_>>>()?
                    .iter()
                    .map(String::as_str),
            )),
            "reasoning_content" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.reasoning_content.as_deref()),
            )),
            "reasoning_effort_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.reasoning_effort.as_ref()),
            )?),
            "metrics_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.metrics.as_ref()),
            )?),
            "model_name" => Arc::new(StringArray::from_iter(
                rows.iter().map(|row| row.model_name.as_deref()),
            )),
            "llm_call_count" => Arc::new(Int64Array::from(
                rows.iter()
                    .map(|row| row.llm_call_count)
                    .collect::<Vec<_>>(),
            )),
            "is_copied_context" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.is_copied_context)
                    .collect::<Vec<_>>(),
            )),
            "latency_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.latency_ms).collect::<Vec<_>>(),
            )),
            "ttft_ms" => Arc::new(Int64Array::from(
                rows.iter().map(|row| row.ttft_ms).collect::<Vec<_>>(),
            )),
            "had_observation" => Arc::new(BooleanArray::from(
                rows.iter()
                    .map(|row| row.had_observation)
                    .collect::<Vec<_>>(),
            )),
            "extra_json" => Arc::new(optional_json_array(
                rows.iter().map(|row| row.extra.as_ref()),
            )?),
            SOURCE_FILE_COLUMN => Arc::new(StringArray::from_iter_values(std::iter::repeat_n(
                relative_path,
                rows.len(),
            ))),
            name => anyhow::bail!("unsupported projected ATIF steps column '{name}'"),
        };
        columns.push(column);
    }
    let options = RecordBatchOptions::new().with_row_count(Some(rows.len()));
    RecordBatch::try_new_with_options(schema, columns, &options)
        .context("build projected ATIF steps batch")
}

fn optional_json_array<'a>(
    values: impl IntoIterator<Item = Option<&'a serde_json::Value>>,
) -> Result<StringArray> {
    Ok(StringArray::from(
        values
            .into_iter()
            .map(|value| value.map(serde_json::to_string).transpose())
            .collect::<serde_json::Result<Vec<_>>>()?,
    ))
}

#[derive(Debug)]
struct FileState {
    file: LocalQueryInputFile,
    load_lock: Mutex<()>,
    live: Mutex<Weak<ParsedFile>>,
}

impl FileState {
    fn new(file: LocalQueryInputFile) -> Self {
        Self {
            file,
            load_lock: Mutex::new(()),
            live: Mutex::new(Weak::new()),
        }
    }
}

#[derive(Debug)]
struct ParsedFile {
    runs: Vec<RecordBatch>,
    steps: Vec<RecordBatch>,
    tool_calls: Vec<RecordBatch>,
    arrow_bytes: usize,
}

impl ParsedFile {
    fn batches(&self, kind: StorylineTableKind) -> &[RecordBatch] {
        match kind {
            StorylineTableKind::Runs => &self.runs,
            StorylineTableKind::Steps => &self.steps,
            StorylineTableKind::ToolCalls => &self.tool_calls,
        }
    }
}

#[derive(Debug)]
struct FileTrajectoryRuntime {
    options: FileTrajectoryDataSourceOptions,
    cache: Mutex<ParsedFileCache>,
    limiter: FileReadLimiter,
    metrics: FileTrajectoryQueryMetrics,
}

impl FileTrajectoryRuntime {
    fn new(options: FileTrajectoryDataSourceOptions) -> Self {
        Self {
            options,
            cache: Mutex::new(ParsedFileCache::default()),
            limiter: FileReadLimiter::new(options.max_concurrent_files),
            metrics: FileTrajectoryQueryMetrics::default(),
        }
    }
}

#[derive(Debug, Default)]
struct ParsedFileCache {
    entries: HashMap<PathBuf, Arc<ParsedFile>>,
    lru: VecDeque<PathBuf>,
    arrow_bytes: usize,
}

impl ParsedFileCache {
    fn get(&mut self, path: &Path) -> Option<Arc<ParsedFile>> {
        let parsed = self.entries.get(path)?.clone();
        self.lru.retain(|candidate| candidate != path);
        self.lru.push_back(path.to_path_buf());
        Some(parsed)
    }

    fn insert(
        &mut self,
        path: PathBuf,
        parsed: Arc<ParsedFile>,
        options: FileTrajectoryDataSourceOptions,
    ) -> u64 {
        if options.cache_files == 0
            || options.cache_bytes == 0
            || parsed.arrow_bytes > options.cache_bytes
        {
            return 0;
        }
        if let Some(previous) = self.entries.remove(&path) {
            self.arrow_bytes = self.arrow_bytes.saturating_sub(previous.arrow_bytes);
            self.lru.retain(|candidate| candidate != &path);
        }
        self.arrow_bytes = self.arrow_bytes.saturating_add(parsed.arrow_bytes);
        self.entries.insert(path.clone(), parsed);
        self.lru.push_back(path);
        let mut evictions = 0;
        while self.entries.len() > options.cache_files || self.arrow_bytes > options.cache_bytes {
            let Some(evicted_path) = self.lru.pop_front() else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&evicted_path) {
                self.arrow_bytes = self.arrow_bytes.saturating_sub(evicted.arrow_bytes);
                evictions += 1;
            }
        }
        evictions
    }
}

#[derive(Debug)]
struct FileReadLimiter {
    available: Mutex<usize>,
    changed: Condvar,
    maximum: usize,
}

impl FileReadLimiter {
    fn new(maximum: usize) -> Self {
        Self {
            available: Mutex::new(maximum),
            changed: Condvar::new(),
            maximum,
        }
    }

    fn acquire(&self) -> Result<FileReadPermit<'_>> {
        let mut available = lock(&self.available, "local query file-read limiter")?;
        while *available == 0 {
            available = self
                .changed
                .wait(available)
                .map_err(|_| anyhow::anyhow!("local query file-read limiter is poisoned"))?;
        }
        *available -= 1;
        Ok(FileReadPermit { limiter: self })
    }
}

struct FileReadPermit<'a> {
    limiter: &'a FileReadLimiter,
}

impl Drop for FileReadPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.limiter.available.lock() {
            *available = (*available + 1).min(self.limiter.maximum);
            self.limiter.changed.notify_one();
        }
    }
}

fn lock<'a, T>(mutex: &'a Mutex<T>, name: &str) -> Result<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| anyhow::anyhow!("{name} is poisoned"))
}

fn load_file(
    state: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    format: FileTrajectoryFormat,
) -> Result<Arc<ParsedFile>> {
    if let Some(parsed) =
        lock(&runtime.cache, "local query parsed-file cache")?.get(state.file.path())
    {
        runtime
            .metrics
            .inner
            .cache_hits
            .fetch_add(1, Ordering::Relaxed);
        return Ok(parsed);
    }
    if let Some(parsed) = lock(&state.live, "local query live-file cache")?.upgrade() {
        runtime
            .metrics
            .inner
            .cache_hits
            .fetch_add(1, Ordering::Relaxed);
        return Ok(parsed);
    }

    let _single_flight = lock(&state.load_lock, "local query per-file load lock")?;
    if let Some(parsed) =
        lock(&runtime.cache, "local query parsed-file cache")?.get(state.file.path())
    {
        runtime
            .metrics
            .inner
            .cache_hits
            .fetch_add(1, Ordering::Relaxed);
        return Ok(parsed);
    }
    if let Some(parsed) = lock(&state.live, "local query live-file cache")?.upgrade() {
        runtime
            .metrics
            .inner
            .cache_hits
            .fetch_add(1, Ordering::Relaxed);
        return Ok(parsed);
    }

    runtime
        .metrics
        .inner
        .cache_misses
        .fetch_add(1, Ordering::Relaxed);

    let _permit = runtime.limiter.acquire()?;
    state.file.validate_unchanged()?;
    anyhow::ensure!(
        state.file.size_bytes() <= runtime.options.max_file_bytes,
        "{} input {} is {} bytes, exceeding max_file_bytes {}",
        format.as_str(),
        state.file.path().display(),
        state.file.size_bytes(),
        runtime.options.max_file_bytes
    );
    let content = fs::read_to_string(state.file.path()).with_context(|| {
        format!(
            "read {} input {}",
            format.as_str(),
            state.file.path().display()
        )
    })?;
    anyhow::ensure!(
        content.len() as u64 <= runtime.options.max_file_bytes,
        "{} input {} exceeded max_file_bytes {} while reading",
        format.as_str(),
        state.file.path().display(),
        runtime.options.max_file_bytes
    );
    state.file.validate_unchanged()?;
    runtime
        .metrics
        .inner
        .source_bytes_read
        .fetch_add(content.len() as u64, Ordering::Relaxed);
    let parsed = Arc::new(parse_file(
        &state.file,
        format,
        &content,
        runtime.options.batch_size,
    )?);
    runtime
        .metrics
        .inner
        .files_parsed
        .fetch_add(1, Ordering::Relaxed);
    *lock(&state.live, "local query live-file cache")? = Arc::downgrade(&parsed);
    let evictions = lock(&runtime.cache, "local query parsed-file cache")?.insert(
        state.file.path().to_path_buf(),
        parsed.clone(),
        runtime.options,
    );
    runtime
        .metrics
        .inner
        .cache_evictions
        .fetch_add(evictions, Ordering::Relaxed);
    Ok(parsed)
}

fn parse_file(
    file: &LocalQueryInputFile,
    format: FileTrajectoryFormat,
    content: &str,
    batch_size: usize,
) -> Result<ParsedFile> {
    let stories = match format {
        FileTrajectoryFormat::Atif => super::atif_datafusion::parse_documents(content)
            .with_context(|| format!("parse ATIF input {}", file.path().display()))?
            .into_iter()
            .map(|trajectory| atif_to_storyline(&trajectory).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>>>()
            .with_context(|| format!("normalize ATIF input {}", file.path().display()))?,
        FileTrajectoryFormat::OpenaiMsg => {
            let value = serde_json::from_str(content)
                .with_context(|| format!("parse OpenAI JSON input {}", file.path().display()))?;
            parse_openai_msg_corpus_value(&value, file.relative_path())
                .map_err(anyhow::Error::from)
                .with_context(|| format!("normalize OpenAI input {}", file.path().display()))?
        }
        FileTrajectoryFormat::Actf => {
            let document = ActfDocument::from_json_str(content)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("parse ACTF input {}", file.path().display()))?;
            actf_to_storylines(&document)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("normalize ACTF input {}", file.path().display()))?
        }
    };

    let mut session_ids = HashSet::with_capacity(stories.len());
    let mut runs = Vec::<StoryRunRow>::with_capacity(stories.len());
    let mut steps = Vec::<StoryStepRow>::new();
    let mut tool_calls = Vec::<StoryToolCallRow>::new();
    for story in stories {
        let tables = split_storyline(&story).map_err(anyhow::Error::from)?;
        anyhow::ensure!(
            session_ids.insert(tables.run.session_id.clone()),
            "duplicate {} session_id '{}' in {}",
            format.as_str(),
            tables.run.session_id,
            file.path().display()
        );
        runs.push(tables.run);
        steps.extend(tables.steps);
        tool_calls.extend(tables.tool_calls);
    }
    anyhow::ensure!(
        !runs.is_empty(),
        "{} input contains no trajectories: {}",
        format.as_str(),
        file.path().display()
    );

    let runs = encode_query_batches(
        &runs,
        batch_size,
        story_runs_to_batch,
        file.relative_path(),
        query_schema(&story_runs_arrow_schema()),
    )?;
    let steps = encode_query_batches(
        &steps,
        batch_size,
        story_steps_to_batch,
        file.relative_path(),
        query_schema(&story_steps_arrow_schema()),
    )?;
    let tool_calls = encode_query_batches(
        &tool_calls,
        batch_size,
        story_tool_calls_to_batch,
        file.relative_path(),
        query_schema(&story_tool_calls_arrow_schema()),
    )?;
    let arrow_bytes = runs
        .iter()
        .chain(&steps)
        .chain(&tool_calls)
        .map(RecordBatch::get_array_memory_size)
        .sum();
    Ok(ParsedFile {
        runs,
        steps,
        tool_calls,
        arrow_bytes,
    })
}

fn encode_query_batches<T>(
    rows: &[T],
    batch_size: usize,
    encode: fn(&[T]) -> Result<RecordBatch>,
    relative_path: &str,
    schema: SchemaRef,
) -> Result<Vec<RecordBatch>> {
    rows.chunks(batch_size)
        .map(|chunk| append_file_column(encode(chunk)?, relative_path, schema.clone()))
        .collect()
}

fn append_file_column(
    base: RecordBatch,
    relative_path: &str,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let mut columns = base.columns().to_vec();
    columns.push(Arc::new(StringArray::from_iter_values(
        std::iter::repeat_n(relative_path, base.num_rows()),
    )));
    RecordBatch::try_new(schema, columns).context("append _file_ to trajectory query batch")
}

fn query_schema(base: &SchemaRef) -> SchemaRef {
    let mut fields = base
        .fields()
        .iter()
        .map(|field| field.as_ref().clone())
        .collect::<Vec<_>>();
    fields.push(Field::new(SOURCE_FILE_COLUMN, DataType::Utf8, false));
    Arc::new(ArrowSchema::new_with_metadata(
        fields,
        base.metadata().clone(),
    ))
}

fn projected_schema(
    schema: &SchemaRef,
    projection: Option<&Vec<usize>>,
) -> datafusion::common::Result<SchemaRef> {
    let Some(projection) = projection else {
        return Ok(schema.clone());
    };
    let fields = projection
        .iter()
        .map(|index| {
            schema.fields().get(*index).cloned().ok_or_else(|| {
                DataFusionError::Plan(format!(
                    "trajectory projection index {index} exceeds schema width {}",
                    schema.fields().len()
                ))
            })
        })
        .collect::<datafusion::common::Result<Vec<_>>>()?;
    Ok(Arc::new(ArrowSchema::new_with_metadata(
        fields,
        schema.metadata().clone(),
    )))
}

fn base_schema(kind: StorylineTableKind) -> SchemaRef {
    match kind {
        StorylineTableKind::Runs => story_runs_arrow_schema(),
        StorylineTableKind::Steps => story_steps_arrow_schema(),
        StorylineTableKind::ToolCalls => story_tool_calls_arrow_schema(),
    }
}

fn atif_step_filters(expr: &Expr) -> Option<Vec<AtifStepFilter>> {
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            let mut left = atif_step_filters(&binary.left)?;
            left.extend(atif_step_filters(&binary.right)?);
            Some(left)
        }
        Expr::BinaryExpr(binary) => {
            if let Some(column) = column_name(&binary.left) {
                return predicate_for_column(column, binary.op, &binary.right)
                    .map(|item| vec![item]);
            }
            let column = column_name(&binary.right)?;
            predicate_for_column(column, reverse_operator(binary.op)?, &binary.left)
                .map(|item| vec![item])
        }
        Expr::Between(between) if column_name(&between.expr) == Some("step_id") => {
            Some(vec![AtifStepFilter::StepId(IntPredicate::Between {
                low: int_literal(&between.low)?,
                high: int_literal(&between.high)?,
                negated: between.negated,
            })])
        }
        Expr::InList(list) => {
            let column = column_name(&list.expr)?;
            match column {
                "session_id" | "source" => {
                    let values = list
                        .list
                        .iter()
                        .map(string_literal)
                        .map(|value| value.map(str::to_string))
                        .collect::<Option<Vec<_>>>()?;
                    let predicate = StringPredicate::In {
                        values,
                        negated: list.negated,
                    };
                    Some(vec![if column == "session_id" {
                        AtifStepFilter::SessionId(predicate)
                    } else {
                        AtifStepFilter::Source(predicate)
                    }])
                }
                "step_id" => Some(vec![AtifStepFilter::StepId(IntPredicate::In {
                    values: list
                        .list
                        .iter()
                        .map(int_literal)
                        .collect::<Option<Vec<_>>>()?,
                    negated: list.negated,
                })]),
                _ => None,
            }
        }
        _ => None,
    }
}

fn predicate_for_column(column: &str, op: Operator, literal: &Expr) -> Option<AtifStepFilter> {
    match column {
        "session_id" | "source" if matches!(op, Operator::Eq | Operator::NotEq) => {
            let value = string_literal(literal)?.to_string();
            let predicate = if op == Operator::Eq {
                StringPredicate::Equal(value)
            } else {
                StringPredicate::NotEqual(value)
            };
            Some(if column == "session_id" {
                AtifStepFilter::SessionId(predicate)
            } else {
                AtifStepFilter::Source(predicate)
            })
        }
        "step_id"
            if matches!(
                op,
                Operator::Eq
                    | Operator::NotEq
                    | Operator::Lt
                    | Operator::LtEq
                    | Operator::Gt
                    | Operator::GtEq
            ) =>
        {
            Some(AtifStepFilter::StepId(IntPredicate::Compare {
                op,
                value: int_literal(literal)?,
            }))
        }
        _ => None,
    }
}

fn reverse_operator(op: Operator) -> Option<Operator> {
    Some(match op {
        Operator::Eq => Operator::Eq,
        Operator::NotEq => Operator::NotEq,
        Operator::Lt => Operator::Gt,
        Operator::LtEq => Operator::GtEq,
        Operator::Gt => Operator::Lt,
        Operator::GtEq => Operator::LtEq,
        _ => return None,
    })
}

fn column_name(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Column(column) => Some(&column.name),
        _ => None,
    }
}

fn int_literal(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Literal(ScalarValue::Int64(Some(value)), _) => Some(*value),
        Expr::Literal(ScalarValue::Int32(Some(value)), _) => Some(i64::from(*value)),
        Expr::Literal(ScalarValue::UInt64(Some(value)), _) => i64::try_from(*value).ok(),
        Expr::Literal(ScalarValue::UInt32(Some(value)), _) => Some(i64::from(*value)),
        _ => None,
    }
}

fn matches_file_filter(expr: &Expr, path: &str) -> Option<bool> {
    match expr {
        Expr::BinaryExpr(binary) if matches!(binary.op, Operator::Eq | Operator::NotEq) => {
            let value = if is_file_column(&binary.left) {
                string_literal(&binary.right)
            } else if is_file_column(&binary.right) {
                string_literal(&binary.left)
            } else {
                None
            }?;
            let equal = path == value;
            Some(if binary.op == Operator::Eq {
                equal
            } else {
                !equal
            })
        }
        Expr::Like(like) if is_file_column(&like.expr) && !like.case_insensitive => {
            let pattern = string_literal(&like.pattern)?;
            let matched = sql_like_matches(path, pattern, like.escape_char)?;
            Some(if like.negated { !matched } else { matched })
        }
        Expr::InList(list) if is_file_column(&list.expr) => {
            let values = list
                .list
                .iter()
                .map(string_literal)
                .collect::<Option<Vec<_>>>()?;
            let contains = values.contains(&path);
            Some(if list.negated { !contains } else { contains })
        }
        _ => None,
    }
}

fn is_file_column(expr: &Expr) -> bool {
    matches!(expr, Expr::Column(column) if column.name == SOURCE_FILE_COLUMN)
}

fn string_literal(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Literal(ScalarValue::Utf8(Some(value)), _)
        | Expr::Literal(ScalarValue::LargeUtf8(Some(value)), _)
        | Expr::Literal(ScalarValue::Utf8View(Some(value)), _) => Some(value),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum LikeToken {
    AnySequence,
    AnyCharacter,
    Literal(char),
}

fn sql_like_matches(value: &str, pattern: &str, escape: Option<char>) -> Option<bool> {
    let tokens = like_tokens(pattern, escape)?;
    let value = value.chars().collect::<Vec<_>>();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in tokens {
        let mut current = vec![false; value.len() + 1];
        match token {
            LikeToken::AnySequence => {
                current[0] = previous[0];
                for index in 1..=value.len() {
                    current[index] = previous[index] || current[index - 1];
                }
            }
            LikeToken::AnyCharacter => {
                current[1..].copy_from_slice(&previous[..value.len()]);
            }
            LikeToken::Literal(expected) => {
                for index in 1..=value.len() {
                    current[index] = previous[index - 1] && value[index - 1] == expected;
                }
            }
        }
        previous = current;
    }
    Some(previous[value.len()])
}

fn like_tokens(pattern: &str, escape: Option<char>) -> Option<Vec<LikeToken>> {
    let mut characters = pattern.chars();
    let mut tokens = Vec::new();
    while let Some(character) = characters.next() {
        if Some(character) == escape {
            let literal = characters.next()?;
            tokens.push(LikeToken::Literal(literal));
        } else {
            tokens.push(match character {
                '%' => LikeToken::AnySequence,
                '_' => LikeToken::AnyCharacter,
                literal => LikeToken::Literal(literal),
            });
        }
    }
    Some(tokens)
}

fn datafusion_error(error: anyhow::Error) -> DataFusionError {
    DataFusionError::Execution(format!("{error:#}"))
}

fn validate_options(options: FileTrajectoryDataSourceOptions) -> Result<()> {
    anyhow::ensure!(
        options.batch_size > 0,
        "local query batch_size must be greater than zero"
    );
    anyhow::ensure!(
        options.max_file_bytes > 0,
        "local query max_file_bytes must be greater than zero"
    );
    anyhow::ensure!(
        options.max_record_bytes > 0,
        "local query max_record_bytes must be greater than zero"
    );
    anyhow::ensure!(
        options.max_concurrent_files > 0,
        "local query max_concurrent_files must be greater than zero"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use datafusion::logical_expr::{col, lit};

    #[test]
    fn virtual_column_does_not_change_lance_schemas() {
        assert!(story_runs_arrow_schema()
            .field_with_name(SOURCE_FILE_COLUMN)
            .is_err());
        assert!(query_schema(&story_runs_arrow_schema())
            .field_with_name(SOURCE_FILE_COLUMN)
            .is_ok());
    }

    #[test]
    fn file_filter_matching_supports_sql_like_and_exact_values() {
        let like = col(SOURCE_FILE_COLUMN).like(lit("batch/%_two.json"));
        assert_eq!(matches_file_filter(&like, "batch/one_two.json"), Some(true));
        assert_eq!(matches_file_filter(&like, "other/two.json"), Some(false));
        let exact = col(SOURCE_FILE_COLUMN).eq(lit("one.json"));
        assert_eq!(matches_file_filter(&exact, "one.json"), Some(true));
        assert_eq!(matches_file_filter(&exact, "two.json"), Some(false));
        assert_eq!(matches_file_filter(&col("session_id"), "one.json"), None);
    }

    #[test]
    fn atif_step_filter_compilation_is_conservative() {
        let filter = col("session_id")
            .eq(lit("run-a"))
            .and(col("step_id").gt_eq(lit(5_i64)))
            .and(col("step_id").lt_eq(lit(15_i64)));
        let compiled = atif_step_filters(&filter).expect("supported conjunction");
        let scan = FileScanSpec {
            projection: Some(Arc::from(vec![1, 2, 6])),
            projected_names: Arc::new(
                ["session_id", "step_id", "source"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            step_filters: Arc::from(compiled),
        };
        assert!(scan.matches_document("run-a"));
        assert!(!scan.matches_document("run-b"));
        assert!(scan.matches_step(5, "agent"));
        assert!(scan.matches_step(15, "agent"));
        assert!(!scan.matches_step(4, "agent"));
        assert!(!scan.matches_step(16, "agent"));
        assert!(atif_step_filters(&col("message_json").eq(lit("x"))).is_none());
    }
}
