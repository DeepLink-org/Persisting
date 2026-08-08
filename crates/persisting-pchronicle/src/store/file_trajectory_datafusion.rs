//! Lazy DataFusion datasource for directly querying OpenAI-message and ACTF JSON.
//!
//! Each source file is one streaming partition. Query-only `_file_` predicates
//! are evaluated against the frozen manifest before partitions are opened.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
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
use lance::deps::arrow_array::{RecordBatch, StringArray};
use lance::deps::arrow_schema::{DataType, Field, Schema as ArrowSchema, SchemaRef};
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
pub const DEFAULT_LOCAL_QUERY_CACHE_BYTES: usize = 256 * 1024 * 1024;
pub const DEFAULT_LOCAL_QUERY_CACHE_FILES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTrajectoryDataSourceOptions {
    pub batch_size: usize,
    /// Hard limit for one source file. The limit is checked before and after reading.
    pub max_file_bytes: u64,
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
        let partitions = self
            .selected_files(filters)
            .into_iter()
            .map(|file| {
                Arc::new(FileTrajectoryPartition {
                    file,
                    runtime: self.runtime.clone(),
                    format: self.format,
                    kind: self.kind,
                    schema: self.schema.clone(),
                    batch_size: self.batch_size,
                }) as Arc<dyn PartitionStream>
            })
            .collect::<Vec<_>>();
        let table = StreamingTable::try_new(self.schema.clone(), partitions)?;
        table.scan(state, projection, &[], limit).await
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
    schema: SchemaRef,
    batch_size: usize,
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
        let schema = self.schema.clone();
        let batch_size = self.batch_size;
        builder.spawn_blocking(move || {
            stream_file(&file, &runtime, format, kind, schema, batch_size, &tx)
                .map_err(datafusion_error)
        });
        builder.build()
    }
}

fn stream_file(
    file: &Arc<FileState>,
    runtime: &Arc<FileTrajectoryRuntime>,
    format: FileTrajectoryFormat,
    kind: StorylineTableKind,
    _schema: SchemaRef,
    _batch_size: usize,
    tx: &Sender<datafusion::common::Result<RecordBatch>>,
) -> Result<()> {
    let parsed = load_file(file, runtime, format)?;
    for batch in parsed.batches(kind) {
        if tx.blocking_send(Ok(batch.clone())).is_err() {
            break;
        }
    }
    Ok(())
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

fn base_schema(kind: StorylineTableKind) -> SchemaRef {
    match kind {
        StorylineTableKind::Runs => story_runs_arrow_schema(),
        StorylineTableKind::Steps => story_steps_arrow_schema(),
        StorylineTableKind::ToolCalls => story_tool_calls_arrow_schema(),
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
            if let Some(literal) = characters.next() {
                tokens.push(LikeToken::Literal(literal));
            } else {
                return None;
            }
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
}
