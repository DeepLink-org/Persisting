//! Lazy DataFusion datasource for directly querying OpenAI-message and ACTF JSON.
//!
//! Each source file is one streaming partition. Query-only `_file_` predicates
//! are evaluated against the frozen manifest before partitions are opened.

mod actf_reader;
mod actf_stream;
mod atif_reader;
mod atif_stream;
mod projected_steps;

use crate::formats::common::json_stream::BoundedCountingReader;
use actf_reader::parse_actf_storylines_from_reader_with_stats;
use actf_stream::{ACTF_TRAJECTORY_NOT_PROJECTABLE, stream_projected_actf_steps};
pub(crate) use atif_reader::AtifReader;
use atif_stream::stream_projected_atif_steps;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};

use anyhow::{Context, Result};
use async_trait::async_trait;
use datafusion::catalog::Session;
use datafusion::catalog::streaming::StreamingTable;
use datafusion::common::ScalarValue;
use datafusion::datasource::TableProvider;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, Operator, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::stream::RecordBatchReceiverStreamBuilder;
use datafusion::physical_plan::streaming::PartitionStream;
use datafusion::physical_plan::{ExecutionPlan, SendableRecordBatchStream};
use datafusion::prelude::SessionContext;
use lance::deps::arrow_array::{RecordBatch, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, SchemaRef};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use tokio::sync::mpsc::Sender;

use crate::format::DocumentFormat;

#[cfg(test)]
use super::story_steps_arrow_schema;
use super::{
    LocalQueryInputFile, LocalQueryManifest, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineDataFusionTableNames, StorylineTableKind,
    datafusion_bridge::{from_datafusion, into_datafusion},
    split_storyline, story_runs_arrow_schema, story_runs_to_batch, story_steps_to_batch,
    story_tool_calls_arrow_schema, story_tool_calls_to_batch,
};
use projected_steps::projected_steps_arrow_schema;

/// Query-only source path column. This is not part of any Lance table schema.
pub const SOURCE_FILE_COLUMN: &str = "_file_";

pub const DEFAULT_LOCAL_QUERY_BATCH_SIZE: usize = 8192;
pub const DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Disable the stricter per-record cap by default; `max_file_bytes` remains authoritative.
pub const DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES: usize = usize::MAX;
pub const DEFAULT_LOCAL_QUERY_CACHE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_LOCAL_QUERY_CACHE_FILES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTrajectoryDataSourceOptions {
    pub batch_size: usize,
    /// Hard limit for one source file. The limit is checked before and after reading.
    pub max_file_bytes: u64,
    /// Optional per-record hard limit. `usize::MAX` disables this stricter cap.
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

#[derive(Debug)]
pub struct FileTrajectoryDataSource {
    format: DocumentFormat,
    runs: Arc<dyn TableProvider>,
    steps: Arc<dyn TableProvider>,
    tool_calls: Arc<dyn TableProvider>,
    file_count: usize,
    max_file_bytes: u64,
    metrics: FileTrajectoryQueryMetrics,
}

impl FileTrajectoryDataSource {
    pub fn from_manifest(manifest: LocalQueryManifest) -> Result<Self> {
        Self::from_manifest_with_options(manifest, FileTrajectoryDataSourceOptions::default())
    }

    pub fn from_manifest_with_options(
        manifest: LocalQueryManifest,
        options: FileTrajectoryDataSourceOptions,
    ) -> Result<Self> {
        validate_options(options)?;
        let format = manifest.format();
        anyhow::ensure!(
            crate::formats::registry::get(format)
                .is_some_and(|handler| handler.capabilities().direct_query),
            "file trajectory datasource does not support '{format}'"
        );
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
            max_file_bytes: options.max_file_bytes,
            metrics,
        })
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn file_count(&self) -> usize {
        self.file_count
    }

    pub(crate) fn max_file_bytes(&self) -> u64 {
        self.max_file_bytes
    }

    pub fn metrics(&self) -> FileTrajectoryQueryMetrics {
        self.metrics.clone()
    }

    pub(crate) fn provider(&self, kind: StorylineTableKind) -> Arc<dyn TableProvider> {
        match kind {
            StorylineTableKind::Runs => self.runs.clone(),
            StorylineTableKind::Steps => self.steps.clone(),
            StorylineTableKind::ToolCalls => self.tool_calls.clone(),
        }
    }

    pub fn register(&self, context: &SessionContext) -> Result<()> {
        self.register_as(context, &StorylineDataFusionTableNames::default())
    }

    pub fn register_as(
        &self,
        context: &SessionContext,
        names: &StorylineDataFusionTableNames,
    ) -> Result<()> {
        validate_table_names(names)?;
        context
            .register_table(&names.runs, self.runs.clone())
            .map_err(|error| from_datafusion("register file-backed runs query table", error))?;
        context
            .register_table(&names.steps, self.steps.clone())
            .map_err(|error| from_datafusion("register file-backed steps query table", error))?;
        context
            .register_table(&names.tool_calls, self.tool_calls.clone())
            .map_err(|error| {
                from_datafusion("register file-backed tool_calls query table", error)
            })?;
        Ok(())
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

    fn can_project_steps(&self, schema: &SchemaRef) -> bool {
        self.projection
            .as_ref()
            .is_some_and(|projection| projection.len() < schema.fields().len())
            && !["turn_ordinal", "had_tool_calls", "observation"]
                .into_iter()
                .any(|name| self.wants(name))
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
    format: DocumentFormat,
    kind: StorylineTableKind,
    schema: SchemaRef,
    batch_size: usize,
}

impl FileTrajectoryTableProvider {
    fn new(
        files: Arc<[Arc<FileState>]>,
        runtime: Arc<FileTrajectoryRuntime>,
        format: DocumentFormat,
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
                } else if matches!(self.format, DocumentFormat::Atif | DocumentFormat::Actf)
                    && self.kind == StorylineTableKind::Steps
                    && atif_step_filters(filter).is_some()
                {
                    // The projected decoder applies these filters to reduce
                    // materialization, while DataFusion retains the filter to
                    // guarantee SQL semantics on the full-normalization path.
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
    format: DocumentFormat,
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
            .map_err(into_datafusion)
        });
        builder.build()
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_file(
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    format: DocumentFormat,
    kind: StorylineTableKind,
    source_schema: SchemaRef,
    schema: SchemaRef,
    batch_size: usize,
    scan: &FileScanSpec,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<()> {
    if format == DocumentFormat::Atif
        && kind == StorylineTableKind::Steps
        && scan.can_project_steps(&source_schema)
    {
        stream_projected_atif_steps(file, runtime, &schema, batch_size, scan, tx)?;
        return Ok(());
    }
    if format == DocumentFormat::Actf
        && kind == StorylineTableKind::Steps
        && scan.can_project_steps(&source_schema)
    {
        match stream_projected_actf_steps(file, runtime, &schema, batch_size, scan, tx) {
            Ok(()) => return Ok(()),
            Err(error) if is_actf_event_log_fallback(&error) => {}
            Err(error) => return Err(error),
        }
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

fn is_actf_event_log_fallback(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains(ACTF_TRAJECTORY_NOT_PROJECTABLE))
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
    format: DocumentFormat,
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
    let parsed = match format {
        DocumentFormat::Actf => {
            let input = File::open(state.file.path()).with_context(|| {
                format!(
                    "open {} input {}",
                    format.as_str(),
                    state.file.path().display()
                )
            })?;
            let mut reader = BufReader::with_capacity(
                64 * 1024,
                BoundedCountingReader::new(input, runtime.options.max_file_bytes),
            );
            let (stories, peak_record_bytes) = parse_actf_storylines_from_reader_with_stats(
                state.file.path(),
                &mut reader,
                runtime.options.max_record_bytes,
            )
            .with_context(|| format!("parse ACTF input {}", state.file.path().display()))?;
            runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                (reader.capacity() as u64).saturating_add(peak_record_bytes as u64),
                Ordering::Relaxed,
            );
            state.file.validate_unchanged()?;
            runtime
                .metrics
                .inner
                .source_bytes_read
                .fetch_add(reader.get_ref().bytes_read(), Ordering::Relaxed);
            stories_to_parsed_file(&state.file, format, stories, runtime.options.batch_size)?
        }
        registered
            if let Some(handler) = crate::formats::registry::get(registered)
                .filter(|handler| handler.capabilities().direct_query) =>
        {
            let input = File::open(state.file.path()).with_context(|| {
                format!(
                    "open {} input {}",
                    format.as_str(),
                    state.file.path().display()
                )
            })?;
            let mut reader = BufReader::with_capacity(
                64 * 1024,
                BoundedCountingReader::new(input, runtime.options.max_file_bytes),
            );
            let source = crate::formats::codec::DocumentSource::new(state.file.relative_path());
            let ctx = crate::formats::codec::DecodeContext::new(&source).with_limits(
                runtime.options.max_file_bytes,
                runtime.options.max_record_bytes,
            );
            let (stories, report) =
                crate::formats::codec::decode_all_with(handler, &mut reader, &ctx)
                    .map_err(anyhow::Error::from)
                    .with_context(|| {
                        format!(
                            "parse {} input {}",
                            format.as_str(),
                            state.file.path().display()
                        )
                    })?;
            state.file.validate_unchanged()?;
            runtime
                .metrics
                .inner
                .source_bytes_read
                .fetch_add(reader.get_ref().bytes_read(), Ordering::Relaxed);
            if report.peak_record_bytes > 0 {
                runtime.metrics.inner.streaming_buffer_peak_bytes.fetch_max(
                    (reader.capacity() as u64).saturating_add(report.peak_record_bytes as u64),
                    Ordering::Relaxed,
                );
            }
            stories_to_parsed_file(&state.file, format, stories, runtime.options.batch_size)?
        }
        unsupported => {
            anyhow::bail!("file trajectory datasource does not support '{unsupported}'")
        }
    };
    let parsed = Arc::new(parsed);
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

fn stories_to_parsed_file(
    file: &LocalQueryInputFile,
    format: DocumentFormat,
    stories: Vec<crate::formats::storyline::StorylineDocument>,
    batch_size: usize,
) -> Result<ParsedFile> {
    let mut document_ids = HashSet::with_capacity(stories.len());
    let mut runs = Vec::<StoryRunRow>::with_capacity(stories.len());
    let mut steps = Vec::<StoryStepRow>::new();
    let mut tool_calls = Vec::<StoryToolCallRow>::new();
    for (ordinal, story) in stories.into_iter().enumerate() {
        let mut tables = split_storyline(&story)?;
        tables.run.storage_ordinal =
            i64::try_from(ordinal).context("local query Storyline storage ordinal overflow")?;
        anyhow::ensure!(
            document_ids.insert(tables.run.document_id.clone()),
            "duplicate {} document_id '{}' in {}",
            format.as_str(),
            tables.run.document_id,
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
        story_runs_to_query_batch,
        file.relative_path(),
        query_schema(&projected_runs_arrow_schema()),
    )?;
    let steps = encode_query_batches(
        &steps,
        batch_size,
        story_steps_to_query_batch,
        file.relative_path(),
        query_schema(&projected_steps_arrow_schema()),
    )?;
    let tool_calls = encode_query_batches(
        &tool_calls,
        batch_size,
        story_tool_calls_to_query_batch,
        file.relative_path(),
        query_schema(&projected_tool_calls_arrow_schema()),
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

fn story_steps_to_query_batch(rows: &[StoryStepRow]) -> Result<RecordBatch> {
    let batch = lance_arrow::json::convert_lance_json_to_arrow(&story_steps_to_batch(rows)?)
        .context("convert direct-query steps JSON columns")?;
    let columns = batch
        .schema()
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, field)| {
            field.name() != "message_kind" && field.name() != "reasoning_effort_kind"
        })
        .map(|(index, _)| batch.column(index).clone())
        .collect::<Vec<_>>();
    RecordBatch::try_new(projected_steps_arrow_schema(), columns)
        .context("build direct-query steps batch")
}

fn projected_runs_arrow_schema() -> SchemaRef {
    // Catalog-backed queries use the same physical Lance JSONB schema for
    // direct files and Storyline stores, allowing mixed-source unions.
    story_runs_arrow_schema()
}

fn story_runs_to_query_batch(rows: &[StoryRunRow]) -> Result<RecordBatch> {
    RecordBatch::try_new(
        projected_runs_arrow_schema(),
        story_runs_to_batch(rows)?.columns().to_vec(),
    )
    .context("build direct-query runs batch")
}

fn story_tool_calls_to_query_batch(rows: &[StoryToolCallRow]) -> Result<RecordBatch> {
    let batch = lance_arrow::json::convert_lance_json_to_arrow(&story_tool_calls_to_batch(rows)?)
        .context("convert direct-query tool-call JSON columns")?;
    RecordBatch::try_new(
        projected_tool_calls_arrow_schema(),
        batch.columns().to_vec(),
    )
    .context("build direct-query tool_calls batch")
}

fn projected_tool_calls_arrow_schema() -> SchemaRef {
    logical_json_schema(&story_tool_calls_arrow_schema())
}

fn logical_json_schema(schema: &SchemaRef) -> SchemaRef {
    let fields = schema
        .fields()
        .iter()
        .map(|field| {
            if lance_arrow::json::is_json_field(field) {
                Field::new(field.name(), DataType::Utf8, field.is_nullable())
            } else {
                field.as_ref().clone()
            }
        })
        .collect::<Vec<_>>();
    Arc::new(ArrowSchema::new_with_metadata(
        fields,
        schema.metadata().clone(),
    ))
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
        StorylineTableKind::Runs => projected_runs_arrow_schema(),
        StorylineTableKind::Steps => projected_steps_arrow_schema(),
        StorylineTableKind::ToolCalls => projected_tool_calls_arrow_schema(),
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

pub(crate) fn matches_file_filter(expr: &Expr, path: &str) -> Option<bool> {
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

fn validate_table_names(names: &StorylineDataFusionTableNames) -> Result<()> {
    let values = [&names.runs, &names.steps, &names.tool_calls];
    for name in values {
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric()),
            "invalid DataFusion table name: {name}"
        );
    }
    anyhow::ensure!(
        names.runs != names.steps
            && names.runs != names.tool_calls
            && names.steps != names.tool_calls,
        "DataFusion table names must be distinct"
    );
    Ok(())
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
mod test_support;
#[cfg(test)]
mod tests;
