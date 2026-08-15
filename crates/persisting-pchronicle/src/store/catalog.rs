//! Query-time, immutable catalogs over one or more trajectory dataset mounts.
//!
//! A catalog is deliberately not a second durable metadata store. Discovery
//! freezes the source membership and version descriptors seen by one
//! query/Web snapshot. Physical sources are opened lazily after catalog-aware
//! `_file_` pruning selects them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use datafusion::arrow::array::{StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::Session;
use datafusion::common::{DataFusionError, ScalarValue, TableReference};
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::logical_expr::{Expr, Operator, TableProviderFilterPushDown, TableType};
use datafusion::physical_expr::expressions::{col as physical_col, Literal};
use datafusion::physical_plan::empty::EmptyExec;
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::projection::{ProjectionExec, ProjectionExpr};
use datafusion::physical_plan::union::UnionExec;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::SessionContext;
use futures::TryStreamExt;
use lance::io::ObjectStore as LanceObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectMeta};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

use crate::convert::{event_storyline_key, project_event_records, storyline_to_events};
use crate::{
    reconstruct_storyline, split_storyline, ChronicleFormat, EventRecord, EventsDocument,
    StoryRunRow, StoryStepRow, StoryToolCallRow, StorylineDocument,
};

use super::file_trajectory_datafusion::matches_file_filter;
use super::raw_event_datafusion::{RawEventDataSourceOptions, RawEventSnapshot};
use super::{
    raw_event_arrow_schema, story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch,
    story_steps_arrow_schema, story_steps_from_batch, story_steps_to_batch,
    story_tool_calls_arrow_schema, story_tool_calls_from_batch, story_tool_calls_to_batch,
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryQueryMetrics,
    LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions, RawEventDataSource,
    StorylineDataSource, StorylineDataSourceOptions, StorylineTableKind, StorylineTablePaths,
    SOURCE_FILE_COLUMN,
};

pub const DEFAULT_DATASET_NAME: &str = "dataset";
pub const CATALOG_SOURCES_TABLE: &str = "sources";
pub const CATALOG_TRAJECTORIES_TABLE: &str = "trajectories";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatasetMount {
    pub name: String,
    pub uri: String,
    #[serde(skip)]
    format_hint: Option<ChronicleFormat>,
}

impl DatasetMount {
    pub fn new(name: impl Into<String>, uri: impl Into<String>) -> Result<Self> {
        let name = normalize_dataset_name(&name.into())?;
        let uri = uri.into();
        anyhow::ensure!(!uri.trim().is_empty(), "dataset URI must not be empty");
        Ok(Self {
            name,
            uri,
            format_hint: None,
        })
    }

    pub fn default(uri: impl Into<String>) -> Result<Self> {
        Self::new(DEFAULT_DATASET_NAME, uri)
    }

    pub fn with_format_hint(mut self, format: ChronicleFormat) -> Self {
        self.format_hint = Some(format);
        self
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogErrorPolicy {
    #[default]
    Strict,
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSourceKind {
    Store,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSourceStatus {
    Ready,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredSource {
    /// Stable path relative to the Dataset mount. A source at the mount root
    /// is represented by `.`.
    pub file: String,
    pub format: Option<String>,
    pub kind: CatalogSourceKind,
    pub snapshot_ref: Option<String>,
    pub size_bytes: Option<u64>,
    pub last_modified: Option<String>,
    pub status: CatalogSourceStatus,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogStorylineKey {
    pub dataset: String,
    pub file: String,
    pub session_id: String,
}

/// One source-consistent trajectory materialization for Web/API consumers.
///
/// Non-event sources normalize the Storyline once and derive the event view
/// from that same document, avoiding a second scan and content hydration pass.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogTrajectoryBundle {
    pub storyline: StorylineDocument,
    pub events: EventsDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogDataset {
    pub mount: DatasetMount,
    pub sources: Vec<DiscoveredSource>,
}

impl CatalogDataset {
    pub fn ready_source_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.status == CatalogSourceStatus::Ready)
            .count()
    }

    pub fn error_source_count(&self) -> usize {
        self.sources.len().saturating_sub(self.ready_source_count())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogSnapshotOptions {
    pub error_policy: CatalogErrorPolicy,
    pub manifest: LocalQueryManifestOptions,
    pub files: FileTrajectoryDataSourceOptions,
    pub storyline: StorylineDataSourceOptions,
}

impl Default for CatalogSnapshotOptions {
    fn default() -> Self {
        Self {
            error_policy: CatalogErrorPolicy::Strict,
            manifest: LocalQueryManifestOptions::default(),
            files: FileTrajectoryDataSourceOptions::default(),
            storyline: StorylineDataSourceOptions::default(),
        }
    }
}

#[derive(Debug)]
pub struct DatasetCatalogSnapshot {
    snapshot_id: String,
    created_at: String,
    default_dataset: Option<String>,
    datasets: Vec<CatalogDataset>,
    prepared: Vec<PreparedDataset>,
    _temporary_files: Arc<SnapshotTempDir>,
}

impl DatasetCatalogSnapshot {
    pub async fn discover(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
    ) -> Result<Self> {
        anyhow::ensure!(!mounts.is_empty(), "mount at least one Dataset");
        validate_catalog_options(options)?;

        let mut names = HashSet::with_capacity(mounts.len());
        for mount in &mounts {
            anyhow::ensure!(
                names.insert(mount.name.clone()),
                "duplicate Dataset name '{}'",
                mount.name
            );
        }
        let default_dataset = default_dataset
            .map(|name| normalize_dataset_name(&name))
            .transpose()?;
        if let Some(default_dataset) = &default_dataset {
            anyhow::ensure!(
                names.contains(default_dataset),
                "default Dataset '{default_dataset}' is not mounted"
            );
        }

        let temporary_files = Arc::new(SnapshotTempDir::new()?);
        let mut datasets = Vec::with_capacity(mounts.len());
        let mut prepared = Vec::with_capacity(mounts.len());
        for mount in mounts {
            let candidates = discover_candidates(&mount, options.manifest).await?;
            let mut source_rows = Vec::with_capacity(candidates.len());
            let mut prepared_sources = Vec::with_capacity(candidates.len());
            for candidate in candidates {
                let stub = candidate.source_stub();
                match freeze_candidate(&mount, candidate, temporary_files.clone(), options).await {
                    Ok((source, lazy_source)) => {
                        source_rows.push(source);
                        prepared_sources.push(lazy_source);
                    }
                    Err(error) if options.error_policy == CatalogErrorPolicy::Report => {
                        source_rows.push(DiscoveredSource {
                            status: CatalogSourceStatus::Error,
                            error: Some(redact_error(&error.to_string())),
                            ..stub
                        });
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("discover Dataset '{}' source '{}'", mount.name, stub.file)
                        });
                    }
                }
            }
            source_rows.sort_by(|left, right| left.file.cmp(&right.file));
            prepared_sources.sort_by(|left, right| left.file().cmp(right.file()));
            datasets.push(CatalogDataset {
                mount: mount.clone(),
                sources: source_rows,
            });
            prepared.push(PreparedDataset {
                name: mount.name,
                sources: prepared_sources,
            });
        }

        let created_at = Utc::now().to_rfc3339();
        let snapshot_id = catalog_snapshot_id(&datasets);
        Ok(Self {
            snapshot_id,
            created_at,
            default_dataset,
            datasets,
            prepared,
            _temporary_files: temporary_files,
        })
    }

    pub fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }

    pub fn created_at(&self) -> &str {
        &self.created_at
    }

    pub fn default_dataset(&self) -> Option<&str> {
        self.default_dataset.as_deref()
    }

    pub fn datasets(&self) -> &[CatalogDataset] {
        &self.datasets
    }

    pub fn dataset(&self, name: &str) -> Option<&CatalogDataset> {
        self.datasets
            .iter()
            .find(|dataset| dataset.mount.name == name)
    }

    pub fn requires_file_join_key(&self) -> bool {
        self.datasets
            .iter()
            .any(|dataset| dataset.ready_source_count() > 1)
    }

    /// Resolve one normalized Storyline without rediscovering its physical source.
    pub async fn load_storyline(
        &self,
        key: &CatalogStorylineKey,
    ) -> Result<Option<StorylineDocument>> {
        let source = self.lazy_source(key)?.resolve().await?;
        load_storyline_from_source(source.as_ref(), key).await
    }

    /// Resolve the normalized Storyline and event view without normalizing a
    /// non-event source twice.
    pub async fn load_trajectory_bundle(
        &self,
        key: &CatalogStorylineKey,
    ) -> Result<Option<CatalogTrajectoryBundle>> {
        let source = self.lazy_source(key)?.resolve().await?;
        if let ResolvedSource::Events(events) = source.as_ref() {
            let normalized = events.normalized().await?;
            let Some(records) = normalized.events_by_session.get(&key.session_id).cloned() else {
                return Ok(None);
            };
            let storyline = load_storyline_from_source(source.as_ref(), key)
                .await?
                .context("canonical events resolved without a normalized Storyline")?;
            return Ok(Some(CatalogTrajectoryBundle {
                storyline,
                events: EventsDocument::new(records),
            }));
        }

        let Some(storyline) = load_storyline_from_source(source.as_ref(), key).await? else {
            return Ok(None);
        };
        let events = storyline_to_events(&storyline)?;
        Ok(Some(CatalogTrajectoryBundle { storyline, events }))
    }

    /// Return canonical records when the source is events.lance, otherwise a
    /// deterministic synthetic event view of the normalized Storyline.
    pub async fn load_events(&self, key: &CatalogStorylineKey) -> Result<Option<EventsDocument>> {
        let source = self.lazy_source(key)?.resolve().await?;
        if let ResolvedSource::Events(source) = source.as_ref() {
            let normalized = source.normalized().await?;
            return Ok(normalized
                .events_by_session
                .get(&key.session_id)
                .cloned()
                .map(EventsDocument::new));
        }
        let Some(storyline) = load_storyline_from_source(source.as_ref(), key).await? else {
            return Ok(None);
        };
        Ok(Some(storyline_to_events(&storyline)?))
    }

    /// Physical canonical events URI for a Storyline source. Non-canonical sources
    /// return `None`; callers must not infer write locations from Dataset mount
    /// roots because a mount may start at any hierarchy level.
    pub fn canonical_event_uri(&self, key: &CatalogStorylineKey) -> Result<Option<&str>> {
        Ok(self.lazy_source(key)?.canonical_event_uri())
    }

    fn lazy_source(&self, key: &CatalogStorylineKey) -> Result<&LazySource> {
        let dataset = normalize_dataset_name(&key.dataset)?;
        let prepared = self
            .prepared
            .iter()
            .find(|candidate| candidate.name == dataset)
            .with_context(|| format!("Dataset '{}' is not mounted", key.dataset))?;
        prepared
            .sources
            .iter()
            .find(|source| source.file() == key.file)
            .map(Arc::as_ref)
            .with_context(|| {
                format!(
                    "Dataset source '{}/{}' is not in snapshot {}",
                    key.dataset, key.file, self.snapshot_id
                )
            })
    }

    pub(crate) fn file_metrics(&self) -> Vec<FileTrajectoryQueryMetrics> {
        self.prepared
            .iter()
            .flat_map(|dataset| &dataset.sources)
            .filter_map(|source| source.file_metrics())
            .collect()
    }

    pub(crate) async fn register(&self, context: &SessionContext) -> Result<()> {
        for (dataset, prepared) in self.datasets.iter().zip(&self.prepared) {
            execute_ddl(
                context,
                &format!("CREATE SCHEMA IF NOT EXISTS {}", dataset.mount.name),
            )
            .await?;
            let is_default = self.default_dataset.as_deref() == Some(dataset.mount.name.as_str());
            let sources = sources_table_provider(&dataset.sources)?;
            register_catalog_provider(
                context,
                &dataset.mount.name,
                CATALOG_SOURCES_TABLE,
                sources,
                is_default,
            )?;
            for kind in CatalogTableKind::ALL {
                let provider: Arc<dyn TableProvider> =
                    Arc::new(CatalogTableProvider::new(prepared.sources.clone(), kind));
                register_catalog_provider(
                    context,
                    &dataset.mount.name,
                    kind.table_name(),
                    provider,
                    is_default,
                )?;
            }
            create_trajectories_view(context, &dataset.mount.name).await?;
            if is_default {
                execute_ddl(
                    context,
                    &format!(
                        "CREATE VIEW {CATALOG_TRAJECTORIES_TABLE} AS SELECT * FROM {}.{CATALOG_TRAJECTORIES_TABLE}",
                        dataset.mount.name
                    ),
                )
                .await?;
            }
        }
        Ok(())
    }
}

async fn load_storyline_from_source(
    source: &ResolvedSource,
    key: &CatalogStorylineKey,
) -> Result<Option<StorylineDocument>> {
    let context = SessionContext::new();
    register_normalized_source(&context, source).await?;
    let session_predicate = sql_string(&key.session_id);
    let run_batches = context
        .sql(&format!(
            "SELECT * FROM runs WHERE session_id = {session_predicate}"
        ))
        .await?
        .collect()
        .await?;
    let mut runs = Vec::new();
    for batch in &run_batches {
        runs.extend(story_runs_from_batch(batch)?);
    }
    if runs.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        runs.len() == 1,
        "Catalog Storyline key resolved {} rows for {}/{}/{}",
        runs.len(),
        key.dataset,
        key.file,
        key.session_id
    );
    let step_batches = context
        .sql(&format!(
            "SELECT * FROM steps WHERE session_id = {session_predicate} ORDER BY step_id"
        ))
        .await?
        .collect()
        .await?;
    let tool_batches = context
        .sql(&format!(
            "SELECT * FROM tool_calls WHERE session_id = {session_predicate} ORDER BY step_id, call_index"
        ))
        .await?
        .collect()
        .await?;
    let mut steps = Vec::new();
    let mut tool_calls = Vec::new();
    for batch in &step_batches {
        steps.extend(story_steps_from_batch(batch)?);
    }
    for batch in &tool_batches {
        tool_calls.extend(story_tool_calls_from_batch(batch)?);
    }
    Ok(Some(reconstruct_storyline(crate::StorylineTables {
        run: runs.remove(0),
        steps,
        tool_calls,
    })?))
}

#[derive(Debug)]
struct SnapshotTempDir {
    path: PathBuf,
}

impl SnapshotTempDir {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "pchronicle-catalog-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir(&path)
            .with_context(|| format!("create catalog temporary directory {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SnapshotTempDir {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("pchronicle-catalog-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
struct PreparedDataset {
    name: String,
    sources: Vec<Arc<LazySource>>,
}

#[derive(Debug)]
struct LazySource {
    file: String,
    spec: LazySourceSpec,
    options: CatalogSnapshotOptions,
    temporary_files: Arc<SnapshotTempDir>,
    resolved: OnceCell<std::result::Result<Arc<ResolvedSource>, String>>,
    resolution_count: AtomicUsize,
}

#[derive(Debug)]
enum LazySourceSpec {
    Storyline {
        paths: StorylineTablePaths,
    },
    Events {
        uri: String,
        snapshot: RawEventSnapshot,
    },
    LocalFile {
        root: PathBuf,
        file: LocalQueryInputFile,
        format_hint: Option<ChronicleFormat>,
    },
    RemoteFile {
        store: Arc<LanceObjectStore>,
        meta: ObjectMeta,
        format_hint: Option<ChronicleFormat>,
    },
}

impl LazySource {
    fn new(
        file: String,
        spec: LazySourceSpec,
        options: CatalogSnapshotOptions,
        temporary_files: Arc<SnapshotTempDir>,
    ) -> Self {
        Self {
            file,
            spec,
            options,
            temporary_files,
            resolved: OnceCell::new(),
            resolution_count: AtomicUsize::new(0),
        }
    }

    fn file(&self) -> &str {
        &self.file
    }

    fn supports(&self, kind: CatalogTableKind) -> bool {
        match (&self.spec, kind) {
            (LazySourceSpec::Events { .. }, _) => true,
            (_, CatalogTableKind::Events) => false,
            _ => true,
        }
    }

    fn canonical_event_uri(&self) -> Option<&str> {
        match &self.spec {
            LazySourceSpec::Events { uri, .. } => Some(uri),
            _ => None,
        }
    }

    async fn resolve(&self) -> Result<Arc<ResolvedSource>> {
        let result = self
            .resolved
            .get_or_init(|| async {
                self.resolution_count.fetch_add(1, Ordering::Relaxed);
                self.resolve_inner()
                    .await
                    .map(Arc::new)
                    .map_err(|error| redact_error(&format!("{error:#}")))
            })
            .await;
        match result {
            Ok(source) => Ok(source.clone()),
            Err(error) => anyhow::bail!("{error}"),
        }
    }

    async fn resolve_inner(&self) -> Result<ResolvedSource> {
        match &self.spec {
            LazySourceSpec::Storyline { paths } => Ok(ResolvedSource::Storyline(
                StorylineDataSource::from_pinned_paths_with_options(
                    paths.clone(),
                    self.options.storyline,
                )
                .await?,
            )),
            LazySourceSpec::Events { snapshot, .. } => {
                let source = RawEventDataSource::from_pinned_snapshot_with_options(
                    snapshot.clone(),
                    RawEventDataSourceOptions::default(),
                )
                .await?;
                Ok(ResolvedSource::Events(ResolvedEventSource {
                    source,
                    normalized: OnceCell::new(),
                    normalization_count: AtomicUsize::new(0),
                }))
            }
            LazySourceSpec::LocalFile {
                root,
                file,
                format_hint,
            } => {
                let format = match format_hint {
                    Some(format) => *format,
                    None => file.detect_format_with_options(self.options.manifest)?,
                };
                let manifest =
                    LocalQueryManifest::from_frozen_files(root, format, vec![file.clone()])?;
                Ok(ResolvedSource::File(
                    FileTrajectoryDataSource::from_manifest_with_options(
                        manifest,
                        self.options.files,
                    )?,
                ))
            }
            LazySourceSpec::RemoteFile {
                store,
                meta,
                format_hint,
            } => {
                let extension = Path::new(&self.file)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("json");
                let local = self.temporary_files.path().join(format!(
                    "remote-{}.{}",
                    uuid::Uuid::new_v4().simple(),
                    extension
                ));
                materialize_pinned_object(store, meta, &local, self.options.files.max_file_bytes)
                    .await
                    .with_context(|| {
                        format!("materialize pinned trajectory object {}", self.file)
                    })?;
                let format = match format_hint {
                    Some(format) => *format,
                    None => LocalQueryManifest::detect_with_options(&local, self.options.manifest)?
                        .format(),
                };
                let manifest = LocalQueryManifest::from_explicit_files(
                    self.temporary_files.path(),
                    format,
                    vec![(local, self.file.clone())],
                )?;
                Ok(ResolvedSource::File(
                    FileTrajectoryDataSource::from_manifest_with_options(
                        manifest,
                        self.options.files,
                    )?,
                ))
            }
        }
    }

    fn file_metrics(&self) -> Option<FileTrajectoryQueryMetrics> {
        match self.resolved.get()?.as_ref().ok()?.as_ref() {
            ResolvedSource::File(source) => Some(source.metrics()),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum ResolvedSource {
    Storyline(StorylineDataSource),
    Events(ResolvedEventSource),
    File(FileTrajectoryDataSource),
}

#[derive(Debug)]
struct ResolvedEventSource {
    source: RawEventDataSource,
    normalized: OnceCell<std::result::Result<Arc<NormalizedEventTables>, String>>,
    normalization_count: AtomicUsize,
}

impl ResolvedEventSource {
    async fn normalized(&self) -> Result<Arc<NormalizedEventTables>> {
        let result = self
            .normalized
            .get_or_init(|| async {
                self.normalization_count.fetch_add(1, Ordering::Relaxed);
                normalize_event_source(&self.source)
                    .await
                    .map(Arc::new)
                    .map_err(|error| redact_error(&format!("{error:#}")))
            })
            .await;
        match result {
            Ok(tables) => Ok(tables.clone()),
            Err(error) => anyhow::bail!("{error}"),
        }
    }
}

struct ResolvedTable {
    provider: Arc<dyn TableProvider>,
    carries_file_column: bool,
}

impl ResolvedSource {
    async fn table(&self, kind: CatalogTableKind) -> Result<Option<ResolvedTable>> {
        let storyline_kind = || match kind {
            CatalogTableKind::Runs => Some(StorylineTableKind::Runs),
            CatalogTableKind::Steps => Some(StorylineTableKind::Steps),
            CatalogTableKind::ToolCalls => Some(StorylineTableKind::ToolCalls),
            CatalogTableKind::Events => None,
        };
        Ok(match self {
            Self::Storyline(source) => storyline_kind().map(|kind| ResolvedTable {
                provider: source.provider(kind),
                carries_file_column: false,
            }),
            Self::File(source) => storyline_kind().map(|kind| ResolvedTable {
                provider: source.provider(kind),
                carries_file_column: true,
            }),
            Self::Events(source) if kind == CatalogTableKind::Events => Some(ResolvedTable {
                provider: source.source.provider(),
                carries_file_column: false,
            }),
            Self::Events(source) => {
                let normalized = source.normalized().await?;
                storyline_kind().map(|kind| ResolvedTable {
                    provider: match kind {
                        StorylineTableKind::Runs => normalized.runs.clone(),
                        StorylineTableKind::Steps => normalized.steps.clone(),
                        StorylineTableKind::ToolCalls => normalized.tool_calls.clone(),
                    },
                    carries_file_column: false,
                })
            }
        })
    }
}

#[derive(Debug)]
struct NormalizedEventTables {
    runs: Arc<MemTable>,
    steps: Arc<MemTable>,
    tool_calls: Arc<MemTable>,
    events_by_session: BTreeMap<String, Vec<EventRecord>>,
}

async fn register_normalized_source(
    context: &SessionContext,
    source: &ResolvedSource,
) -> Result<()> {
    match source {
        ResolvedSource::Storyline(source) => source.register(context),
        ResolvedSource::File(source) => source.register(context),
        ResolvedSource::Events(source) => {
            let normalized = source.normalized().await?;
            context.register_table("runs", normalized.runs.clone())?;
            context.register_table("steps", normalized.steps.clone())?;
            context.register_table("tool_calls", normalized.tool_calls.clone())?;
            Ok(())
        }
    }
}

#[derive(Debug)]
enum Candidate {
    Storyline {
        file: String,
        uri: String,
        size_bytes: Option<u64>,
        last_modified: Option<String>,
    },
    Events {
        file: String,
        uri: String,
        size_bytes: Option<u64>,
        last_modified: Option<String>,
    },
    LocalFile {
        file: String,
        root: PathBuf,
        path: PathBuf,
        size_bytes: u64,
        last_modified: Option<String>,
    },
    RemoteFile {
        file: String,
        store: Arc<LanceObjectStore>,
        meta: ObjectMeta,
    },
}

impl Candidate {
    fn source_stub(&self) -> DiscoveredSource {
        let (file, format, kind, size_bytes, last_modified, snapshot_ref) = match self {
            Self::Storyline {
                file,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                Some(ChronicleFormat::Storyline.as_str().to_string()),
                CatalogSourceKind::Store,
                *size_bytes,
                last_modified.clone(),
                None,
            ),
            Self::Events {
                file,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                Some(ChronicleFormat::Events.as_str().to_string()),
                CatalogSourceKind::Store,
                *size_bytes,
                last_modified.clone(),
                None,
            ),
            Self::LocalFile {
                file,
                path,
                size_bytes,
                last_modified,
                ..
            } => (
                file.clone(),
                None,
                CatalogSourceKind::File,
                Some(*size_bytes),
                last_modified.clone(),
                Some(local_snapshot_ref(path)),
            ),
            Self::RemoteFile { file, meta, .. } => (
                file.clone(),
                None,
                CatalogSourceKind::File,
                Some(meta.size),
                Some(meta.last_modified.to_rfc3339()),
                Some(remote_snapshot_ref(meta)),
            ),
        };
        DiscoveredSource {
            file,
            format,
            kind,
            snapshot_ref,
            size_bytes,
            last_modified,
            status: CatalogSourceStatus::Ready,
            error: None,
        }
    }
}

async fn freeze_candidate(
    mount: &DatasetMount,
    candidate: Candidate,
    temporary_files: Arc<SnapshotTempDir>,
    options: CatalogSnapshotOptions,
) -> Result<(DiscoveredSource, Arc<LazySource>)> {
    let mut source_row = candidate.source_stub();
    match candidate {
        Candidate::Storyline { file, uri, .. } => {
            ensure_format_hint(mount, ChronicleFormat::Storyline, &file)?;
            let paths = StorylineDataSource::pin_uri(&uri)
                .await
                .with_context(|| format!("pin Storyline source {uri}"))?;
            source_row.snapshot_ref = Some(paths.generation.clone());
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::Storyline { paths },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::Events { file, uri, .. } => {
            ensure_format_hint(mount, ChronicleFormat::Events, &file)?;
            let snapshot = RawEventDataSource::pin_uri(&uri)
                .await
                .with_context(|| format!("pin canonical event source {uri}"))?;
            source_row.snapshot_ref = Some(format!("manifest-revision:{}", snapshot.version()));
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::Events { uri, snapshot },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::LocalFile {
            file, root, path, ..
        } => {
            // Keep format detection behind LazySource::resolve so an exact
            // `_file_` predicate can prune unrelated malformed files before
            // any of their contents are opened.
            source_row.format = mount.format_hint.map(|format| format.as_str().to_string());
            let frozen_file = LocalQueryInputFile::freeze(path, file.clone())?;
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::LocalFile {
                        root,
                        file: frozen_file,
                        format_hint: mount.format_hint,
                    },
                    options,
                    temporary_files,
                )),
            ))
        }
        Candidate::RemoteFile { file, store, meta } => {
            anyhow::ensure!(
                meta.size <= options.manifest.max_detection_bytes,
                "format detection input {file} is {} bytes, exceeding max_detection_bytes {}",
                meta.size,
                options.manifest.max_detection_bytes
            );
            anyhow::ensure!(
                meta.size <= options.files.max_file_bytes,
                "trajectory query file {file} is {} bytes, exceeding max_file_bytes {}",
                meta.size,
                options.files.max_file_bytes
            );
            source_row.format = mount.format_hint.map(|format| format.as_str().to_string());
            Ok((
                source_row,
                Arc::new(LazySource::new(
                    file,
                    LazySourceSpec::RemoteFile {
                        store,
                        meta,
                        format_hint: mount.format_hint,
                    },
                    options,
                    temporary_files,
                )),
            ))
        }
    }
}

fn ensure_format_hint(mount: &DatasetMount, actual: ChronicleFormat, file: &str) -> Result<()> {
    if let Some(expected) = mount.format_hint {
        anyhow::ensure!(
            expected == actual,
            "Dataset source {file} is {actual}, but --source selected {expected}"
        );
    }
    Ok(())
}

async fn normalize_event_source(source: &RawEventDataSource) -> Result<NormalizedEventTables> {
    let records = source.read_records_in_append_order().await?;
    let mut groups = BTreeMap::<String, Vec<EventRecord>>::new();
    for record in records {
        let key = event_storyline_key(&record)
            .context("canonical event cannot be projected without a Storyline identity")?;
        groups.entry(key.to_string()).or_default().push(record);
    }

    let mut runs = Vec::<StoryRunRow>::new();
    let mut steps = Vec::<StoryStepRow>::new();
    let mut tool_calls = Vec::<StoryToolCallRow>::new();
    let mut events_by_session = BTreeMap::new();
    for (group_key, records) in groups {
        let story = project_event_records(&records)?;
        anyhow::ensure!(
            story.session_id == group_key,
            "projected Storyline identity changed"
        );
        let tables = split_storyline(&story)?;
        anyhow::ensure!(
            events_by_session
                .insert(tables.run.session_id.clone(), records)
                .is_none(),
            "canonical events produced a duplicate Catalog session_id '{}'",
            tables.run.session_id
        );
        runs.push(tables.run);
        steps.extend(tables.steps);
        tool_calls.extend(tables.tool_calls);
    }
    let run_batch = story_runs_to_batch(&runs)?;
    let step_batch = story_steps_to_batch(&steps)?;
    let tool_batch = story_tool_calls_to_batch(&tool_calls)?;
    Ok(NormalizedEventTables {
        runs: Arc::new(MemTable::try_new(
            story_runs_arrow_schema(),
            vec![vec![run_batch]],
        )?),
        steps: Arc::new(MemTable::try_new(
            story_steps_arrow_schema(),
            vec![vec![step_batch]],
        )?),
        tool_calls: Arc::new(MemTable::try_new(
            story_tool_calls_arrow_schema(),
            vec![vec![tool_batch]],
        )?),
        events_by_session,
    })
}

async fn discover_candidates(
    mount: &DatasetMount,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    if let Some(path) = local_mount_path(&mount.uri) {
        discover_local_candidates(&mount.uri, &path, options)
    } else {
        discover_object_candidates(&mount.uri, options).await
    }
}

fn discover_local_candidates(
    original_uri: &str,
    root: &Path,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    anyhow::ensure!(
        options.max_entries > 0,
        "catalog max_entries must be positive"
    );
    anyhow::ensure!(options.max_files > 0, "catalog max_files must be positive");
    anyhow::ensure!(
        root.exists(),
        "Dataset input does not exist: {original_uri}"
    );
    if root.is_file() {
        anyhow::ensure!(
            is_json_candidate(root),
            "unsupported Dataset file: {original_uri}"
        );
        let metadata = fs::metadata(root)?;
        return Ok(vec![Candidate::LocalFile {
            file: root
                .file_name()
                .and_then(|name| name.to_str())
                .context("Dataset input filename is not UTF-8")?
                .to_string(),
            root: root
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf(),
            path: root.to_path_buf(),
            size_bytes: metadata.len(),
            last_modified: modified_string(&metadata),
        }]);
    }
    anyhow::ensure!(
        root.is_dir(),
        "Dataset input is not a directory: {original_uri}"
    );

    if root.join("CURRENT").is_file() {
        let metadata = fs::metadata(root.join("CURRENT"))?;
        return Ok(vec![Candidate::Storyline {
            file: ".".into(),
            uri: canonical_local_uri(root)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]);
    }
    if root.join("_manifest.json").is_file()
        && root.file_name().is_some_and(|name| name == "events.lance")
    {
        let metadata = fs::metadata(root.join("_manifest.json"))?;
        return Ok(vec![Candidate::Events {
            file: ".".into(),
            uri: canonical_local_uri(root)?,
            size_bytes: Some(metadata.len()),
            last_modified: modified_string(&metadata),
        }]);
    }

    let mut candidates = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .with_context(|| format!("read Dataset directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            visited = visited.saturating_add(1);
            anyhow::ensure!(
                visited <= options.max_entries,
                "Dataset traversal exceeds max_entries limit of {}",
                options.max_entries
            );
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if path.join("CURRENT").is_file() {
                    let metadata = fs::metadata(path.join("CURRENT"))?;
                    candidates.push(Candidate::Storyline {
                        file: relative_catalog_path(root, &path, true)?,
                        uri: canonical_local_uri(&path)?,
                        size_bytes: Some(metadata.len()),
                        last_modified: modified_string(&metadata),
                    });
                } else if path.join("_manifest.json").is_file()
                    && path.file_name().is_some_and(|name| name == "events.lance")
                {
                    let metadata = fs::metadata(path.join("_manifest.json"))?;
                    candidates.push(Candidate::Events {
                        file: relative_catalog_path(root, &path, true)?,
                        uri: canonical_local_uri(&path)?,
                        size_bytes: Some(metadata.len()),
                        last_modified: modified_string(&metadata),
                    });
                } else if is_lance_directory(&path) {
                    // Derived Lance datasets such as judgments.lance and
                    // revisions.lance are sidecars of a canonical Run, not
                    // trajectory sources. Never descend into their internal
                    // JSON metadata and register it as an outer file source.
                } else {
                    pending.push(path);
                }
            } else if file_type.is_file() && is_json_candidate(&path) {
                let metadata = entry.metadata()?;
                candidates.push(Candidate::LocalFile {
                    file: relative_catalog_path(root, &path, false)?,
                    root: root.to_path_buf(),
                    path,
                    size_bytes: metadata.len(),
                    last_modified: modified_string(&metadata),
                });
            }
            anyhow::ensure!(
                candidates.len() <= options.max_files,
                "Dataset manifest exceeds max_files limit of {}",
                options.max_files
            );
        }
    }
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

async fn discover_object_candidates(
    uri: &str,
    options: LocalQueryManifestOptions,
) -> Result<Vec<Candidate>> {
    let (store, root) = LanceObjectStore::from_uri(uri)
        .await
        .with_context(|| format!("open Dataset object store {uri}"))?;
    let store = Arc::clone(&store);
    let mut metas = store
        .inner
        .list(Some(&root))
        .try_collect::<Vec<_>>()
        .await
        .with_context(|| format!("list Dataset object prefix {uri}"))?;
    anyhow::ensure!(
        metas.len() <= options.max_entries,
        "Dataset traversal exceeds max_entries limit of {}",
        options.max_entries
    );
    metas.sort_by(|left, right| left.location.cmp(&right.location));

    let root_is_events = root.as_ref().ends_with("events.lance");
    let mut storyline_roots = BTreeMap::<String, ObjectMeta>::new();
    let mut event_roots = BTreeMap::<String, ObjectMeta>::new();
    let mut relative_metas = Vec::with_capacity(metas.len());
    for meta in metas {
        let relative = relative_object_path(&root, &meta.location)?;
        if relative == "CURRENT" || relative.ends_with("/CURRENT") {
            storyline_roots.insert(parent_relative_path(&relative, "CURRENT"), meta.clone());
        }
        if (relative == "_manifest.json" && root_is_events)
            || relative.ends_with("/events.lance/_manifest.json")
        {
            event_roots.insert(
                parent_relative_path(&relative, "_manifest.json"),
                meta.clone(),
            );
        }
        relative_metas.push((relative, meta));
    }

    let mut candidates = Vec::new();
    for (relative, meta) in &storyline_roots {
        candidates.push(Candidate::Storyline {
            file: root_source_path(relative),
            uri: child_uri(uri, relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified.to_rfc3339()),
        });
    }
    for (relative, meta) in &event_roots {
        if is_nested_in_any(relative, storyline_roots.keys()) {
            continue;
        }
        candidates.push(Candidate::Events {
            file: root_source_path(relative),
            uri: child_uri(uri, relative),
            size_bytes: Some(meta.size),
            last_modified: Some(meta.last_modified.to_rfc3339()),
        });
    }

    let composite_roots = storyline_roots
        .keys()
        .chain(event_roots.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for (relative, meta) in relative_metas {
        if is_nested_in_any(&relative, composite_roots.iter())
            || path_is_inside_lance_directory(&relative)
        {
            continue;
        }
        let candidate_path = if relative.is_empty() {
            Path::new(root.as_ref())
        } else {
            Path::new(&relative)
        };
        if is_json_candidate(candidate_path) {
            let file = if relative.is_empty() {
                root.as_ref()
                    .rsplit('/')
                    .next()
                    .unwrap_or("dataset.json")
                    .to_string()
            } else {
                relative
            };
            candidates.push(Candidate::RemoteFile {
                file,
                store: Arc::clone(&store),
                meta,
            });
        }
    }
    anyhow::ensure!(
        candidates.len() <= options.max_files,
        "Dataset manifest exceeds max_files limit of {}",
        options.max_files
    );
    candidates.sort_by(|left, right| left.source_stub().file.cmp(&right.source_stub().file));
    Ok(candidates)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogTableKind {
    Runs,
    Steps,
    ToolCalls,
    Events,
}

impl CatalogTableKind {
    const ALL: [Self; 4] = [Self::Runs, Self::Steps, Self::ToolCalls, Self::Events];

    fn table_name(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Steps => "steps",
            Self::ToolCalls => "tool_calls",
            Self::Events => "events",
        }
    }

    fn base_schema(self) -> SchemaRef {
        match self {
            Self::Runs => story_runs_arrow_schema(),
            Self::Steps => story_steps_arrow_schema(),
            Self::ToolCalls => story_tool_calls_arrow_schema(),
            Self::Events => raw_event_arrow_schema(),
        }
    }
}

/// One Dataset-level provider per stable table. It evaluates catalog-owned
/// `_file_` predicates before resolving a source, then delegates the remaining
/// projection/filter/limit pushdown to that source's native provider.
#[derive(Debug)]
struct CatalogTableProvider {
    sources: Vec<Arc<LazySource>>,
    kind: CatalogTableKind,
    schema: SchemaRef,
}

impl CatalogTableProvider {
    fn new(sources: Vec<Arc<LazySource>>, kind: CatalogTableKind) -> Self {
        Self {
            sources,
            kind,
            schema: catalog_schema(&kind.base_schema()),
        }
    }
}

#[async_trait]
impl TableProvider for CatalogTableProvider {
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
        let output_schema = projected_schema(&self.schema, projection)?;
        let mut plans = Vec::<Arc<dyn ExecutionPlan>>::new();
        for source in self.sources.iter().filter(|source| {
            source.supports(self.kind)
                && filters
                    .iter()
                    .all(|filter| evaluate_file_filter(filter, source.file()).unwrap_or(true))
        }) {
            // `report` only applies while freezing candidate descriptors. Once
            // a source is recorded as ready in the immutable snapshot, a late
            // resolution error must fail the query instead of silently
            // producing an incomplete result.
            let resolved = source.resolve().await.map_err(|error| {
                DataFusionError::Execution(format!(
                    "resolve Dataset source '{}': {error:#}",
                    source.file()
                ))
            })?;
            let Some(table) = resolved.table(self.kind).await.map_err(|error| {
                DataFusionError::Execution(format!(
                    "prepare Dataset source '{}' table {}: {error:#}",
                    source.file(),
                    self.kind.table_name()
                ))
            })?
            else {
                continue;
            };
            let plan = if table.carries_file_column {
                let source_projection =
                    file_source_projection(projection, self.schema.fields().len());
                table
                    .provider
                    .scan(state, Some(&source_projection), filters, limit)
                    .await?
            } else {
                let base_projection = base_projection(projection);
                let business_filters = business_filters(filters);
                let input = table
                    .provider
                    .scan(state, base_projection.as_ref(), &business_filters, limit)
                    .await?;
                project_catalog_source(input, source.file(), projection, &self.schema)?
            };
            plans.push(plan);
        }

        let selected_source_count = plans.len();
        let plan: Arc<dyn ExecutionPlan> = match selected_source_count {
            0 => Arc::new(EmptyExec::new(output_schema)),
            1 => plans.pop().expect("one Catalog source plan"),
            _ => UnionExec::try_new(plans)?,
        };
        Ok(match limit {
            Some(limit) if selected_source_count > 1 => {
                Arc::new(GlobalLimitExec::new(plan, 0, Some(limit)))
            }
            _ => plan,
        })
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::common::Result<Vec<TableProviderFilterPushDown>> {
        Ok(filters
            .iter()
            .map(|filter| {
                let columns = filter.column_refs();
                if !columns.is_empty()
                    && columns
                        .iter()
                        .all(|column| column.name == SOURCE_FILE_COLUMN)
                    && evaluate_file_filter(filter, "").is_some()
                {
                    TableProviderFilterPushDown::Exact
                } else {
                    // The provider still forwards safe business predicates to
                    // native sources, while DataFusion retains the expression
                    // above us for cross-format semantic correctness.
                    TableProviderFilterPushDown::Inexact
                }
            })
            .collect())
    }
}

fn evaluate_file_filter(expr: &Expr, path: &str) -> Option<bool> {
    if let Some(value) = matches_file_filter(expr, path) {
        return Some(value);
    }
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            let left = evaluate_file_filter(&binary.left, path);
            let right = evaluate_file_filter(&binary.right, path);
            match (left, right) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            }
        }
        Expr::BinaryExpr(binary) if binary.op == Operator::Or => {
            let left = evaluate_file_filter(&binary.left, path);
            let right = evaluate_file_filter(&binary.right, path);
            match (left, right) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            }
        }
        Expr::Not(inner) => evaluate_file_filter(inner, path).map(|value| !value),
        Expr::Literal(ScalarValue::Boolean(value), _) => *value,
        _ => None,
    }
}

fn business_filters(filters: &[Expr]) -> Vec<Expr> {
    let mut output = Vec::new();
    for filter in filters {
        collect_business_conjuncts(filter, &mut output);
    }
    output
}

fn collect_business_conjuncts(expr: &Expr, output: &mut Vec<Expr>) {
    if !expr
        .column_refs()
        .iter()
        .any(|column| column.name == SOURCE_FILE_COLUMN)
    {
        output.push(expr.clone());
    } else if let Expr::BinaryExpr(binary) = expr {
        if binary.op == Operator::And {
            collect_business_conjuncts(&binary.left, output);
            collect_business_conjuncts(&binary.right, output);
        }
    }
}

fn base_projection(projection: Option<&Vec<usize>>) -> Option<Vec<usize>> {
    projection.map(|projection| {
        projection
            .iter()
            .filter_map(|index| index.checked_sub(1))
            .collect()
    })
}

fn file_source_projection(projection: Option<&Vec<usize>>, catalog_width: usize) -> Vec<usize> {
    let file_column = catalog_width.saturating_sub(1);
    projection
        .cloned()
        .unwrap_or_else(|| (0..catalog_width).collect())
        .into_iter()
        .map(|index| if index == 0 { file_column } else { index - 1 })
        .collect()
}

fn projected_schema(
    schema: &SchemaRef,
    projection: Option<&Vec<usize>>,
) -> datafusion::common::Result<SchemaRef> {
    Ok(match projection {
        Some(projection) => Arc::new(schema.project(projection)?),
        None => schema.clone(),
    })
}

fn project_catalog_source(
    input: Arc<dyn ExecutionPlan>,
    file: &str,
    projection: Option<&Vec<usize>>,
    schema: &SchemaRef,
) -> datafusion::common::Result<Arc<dyn ExecutionPlan>> {
    let requested = projection
        .cloned()
        .unwrap_or_else(|| (0..schema.fields().len()).collect());
    let expressions = requested
        .into_iter()
        .map(|index| {
            let field = schema.field(index);
            let expr: Arc<dyn datafusion::physical_expr::PhysicalExpr> = if index == 0 {
                Arc::new(Literal::new(ScalarValue::Utf8(Some(file.to_string()))))
            } else {
                physical_col(field.name(), input.schema().as_ref())?
            };
            Ok(ProjectionExpr {
                expr,
                alias: field.name().clone(),
            })
        })
        .collect::<datafusion::common::Result<Vec<_>>>()?;
    Ok(Arc::new(ProjectionExec::try_new(expressions, input)?))
}

fn register_catalog_provider(
    context: &SessionContext,
    dataset: &str,
    table: &str,
    provider: Arc<dyn TableProvider>,
    register_default: bool,
) -> Result<()> {
    context.register_table(
        TableReference::partial(dataset.to_string(), table.to_string()),
        provider.clone(),
    )?;
    if register_default {
        context.register_table(TableReference::bare(table.to_string()), provider)?;
    }
    Ok(())
}

async fn create_trajectories_view(context: &SessionContext, dataset: &str) -> Result<()> {
    execute_ddl(
        context,
        &format!(
            "CREATE VIEW {dataset}.trajectories AS \
             SELECT r.*, \
                    (SELECT COUNT(*) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_count, \
                    (SELECT array_agg(s.step_id ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_ids, \
                    (SELECT array_agg(s.source ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS step_sources, \
                    (SELECT array_agg(s.message_json ORDER BY s.step_id) FROM {dataset}.steps s \
                      WHERE s._file_ = r._file_ AND s.session_id = r.session_id) AS messages_json, \
                    (SELECT COUNT(*) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_call_count, \
                    (SELECT array_agg(t.function_name ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_names, \
                    (SELECT array_agg(t.arguments_json ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_arguments_json, \
                    (SELECT array_agg(t.results_json ORDER BY t.step_id, t.call_index) FROM {dataset}.tool_calls t \
                      WHERE t._file_ = r._file_ AND t.session_id = r.session_id) AS tool_results_json \
             FROM {dataset}.runs r"
        ),
    )
    .await
}

async fn execute_ddl(context: &SessionContext, sql: &str) -> Result<()> {
    context
        .sql(sql)
        .await
        .with_context(|| format!("plan Catalog DDL: {sql}"))?
        .collect()
        .await
        .with_context(|| format!("execute Catalog DDL: {sql}"))?;
    Ok(())
}

fn sources_table_provider(sources: &[DiscoveredSource]) -> Result<Arc<dyn TableProvider>> {
    let schema = sources_schema();
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from_iter_values(
                sources.iter().map(|source| source.file.as_str()),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.format.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from_iter_values(sources.iter().map(
                |source| match source.kind {
                    CatalogSourceKind::Store => "store",
                    CatalogSourceKind::File => "file",
                },
            ))),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.snapshot_ref.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(UInt64Array::from(
                sources
                    .iter()
                    .map(|source| source.size_bytes)
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.last_modified.as_deref())
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from_iter_values(sources.iter().map(
                |source| match source.status {
                    CatalogSourceStatus::Ready => "ready",
                    CatalogSourceStatus::Error => "error",
                },
            ))),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.error.as_deref())
                    .collect::<Vec<_>>(),
            )),
        ],
    )?;
    Ok(Arc::new(MemTable::try_new(schema, vec![vec![batch]])?))
}

fn sources_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new(SOURCE_FILE_COLUMN, DataType::Utf8, false),
        Field::new("format", DataType::Utf8, true),
        Field::new("kind", DataType::Utf8, false),
        Field::new("snapshot_ref", DataType::Utf8, true),
        Field::new("size_bytes", DataType::UInt64, true),
        Field::new("last_modified", DataType::Utf8, true),
        Field::new("status", DataType::Utf8, false),
        Field::new("error", DataType::Utf8, true),
    ]))
}

fn catalog_schema(base: &SchemaRef) -> SchemaRef {
    let mut fields = Vec::with_capacity(base.fields().len() + 1);
    fields.push(Arc::new(Field::new(
        SOURCE_FILE_COLUMN,
        DataType::Utf8,
        false,
    )));
    fields.extend(base.fields().iter().cloned());
    Arc::new(Schema::new(fields))
}

async fn materialize_pinned_object(
    store: &Arc<LanceObjectStore>,
    meta: &ObjectMeta,
    destination: &Path,
    max_bytes: u64,
) -> Result<()> {
    let options = GetOptions {
        if_match: meta.e_tag.clone(),
        version: meta.version.clone(),
        ..GetOptions::default()
    };
    let mut stream = store
        .inner
        .get_opts(&meta.location, options)
        .await
        .with_context(|| format!("read pinned Dataset object {}", meta.location))?
        .into_stream();
    let mut output = tokio::fs::File::create(destination)
        .await
        .with_context(|| format!("create pinned Dataset file {}", destination.display()))?;
    let mut written = 0u64;
    while let Some(chunk) = stream
        .try_next()
        .await
        .with_context(|| format!("stream pinned Dataset object {}", meta.location))?
    {
        written = written.saturating_add(chunk.len() as u64);
        anyhow::ensure!(
            written <= max_bytes,
            "pinned Dataset object {} exceeds max_file_bytes {max_bytes}",
            meta.location
        );
        output
            .write_all(&chunk)
            .await
            .with_context(|| format!("write pinned Dataset file {}", destination.display()))?;
    }
    output
        .flush()
        .await
        .with_context(|| format!("flush pinned Dataset file {}", destination.display()))?;
    anyhow::ensure!(
        written == meta.size,
        "object {} size changed while freezing Dataset snapshot",
        meta.location
    );
    Ok(())
}

fn validate_catalog_options(options: CatalogSnapshotOptions) -> Result<()> {
    anyhow::ensure!(
        options.manifest.max_files > 0,
        "catalog max_files must be positive"
    );
    anyhow::ensure!(
        options.manifest.max_entries > 0,
        "catalog max_entries must be positive"
    );
    anyhow::ensure!(
        options.manifest.max_detection_bytes > 0,
        "catalog max_detection_bytes must be positive"
    );
    anyhow::ensure!(
        options.files.max_file_bytes > 0,
        "catalog max_file_bytes must be positive"
    );
    Ok(())
}

fn normalize_dataset_name(name: &str) -> Result<String> {
    let name = name.trim().to_ascii_lowercase();
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    let valid_rest =
        characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    anyhow::ensure!(
        valid_start && valid_rest,
        "Dataset name '{name}' must match [A-Za-z_][A-Za-z0-9_]*"
    );
    anyhow::ensure!(
        !matches!(name.as_str(), "public" | "information_schema"),
        "Dataset name '{name}' is reserved"
    );
    Ok(name)
}

fn local_mount_path(uri: &str) -> Option<PathBuf> {
    if let Some(path) = uri.strip_prefix("local://") {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = uri.strip_prefix("file://") {
        return Some(PathBuf::from(path));
    }
    (!uri.contains("://")).then(|| PathBuf::from(uri))
}

fn is_json_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json" | "jsonl" | "ndjson"
            )
        })
}

fn is_lance_directory(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lance"))
}

fn path_is_inside_lance_directory(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| match component {
            std::path::Component::Normal(name) => Path::new(name)
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lance")),
            _ => false,
        })
}

fn relative_catalog_path(root: &Path, path: &Path, allow_root: bool) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("make {} relative to {}", path.display(), root.display()))?;
    if relative.as_os_str().is_empty() && allow_root {
        return Ok(".".into());
    }
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .context("Dataset source path is not UTF-8"),
            _ => anyhow::bail!("Dataset source path is not safely relative"),
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!components.is_empty(), "Dataset source path is empty");
    Ok(components.join("/"))
}

fn modified_string(metadata: &fs::Metadata) -> Option<String> {
    metadata
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|value| value.to_rfc3339())
}

fn canonical_local_uri(path: &Path) -> Result<String> {
    Ok(fs::canonicalize(path)
        .with_context(|| format!("canonicalize Dataset source {}", path.display()))?
        .to_string_lossy()
        .into_owned())
}

fn local_snapshot_ref(path: &Path) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(path.to_string_lossy().as_bytes());
    if let Ok(metadata) = fs::metadata(path) {
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

fn remote_snapshot_ref(meta: &ObjectMeta) -> String {
    if let Some(version) = &meta.version {
        format!("version:{version}")
    } else if let Some(etag) = &meta.e_tag {
        format!("etag:{etag}")
    } else {
        format!(
            "object:{}:{}:{}",
            meta.size,
            meta.last_modified.timestamp(),
            meta.location
        )
    }
}

fn relative_object_path(root: &ObjectPath, location: &ObjectPath) -> Result<String> {
    let root = root.as_ref().trim_matches('/');
    let location = location.as_ref().trim_matches('/');
    if root.is_empty() {
        return Ok(location.to_string());
    }
    if location == root {
        return Ok(String::new());
    }
    location
        .strip_prefix(root)
        .and_then(|relative| relative.strip_prefix('/'))
        .map(str::to_owned)
        .with_context(|| format!("object {location} is outside Dataset prefix {root}"))
}

fn parent_relative_path(path: &str, leaf: &str) -> String {
    path.strip_suffix(leaf)
        .unwrap_or(path)
        .trim_end_matches('/')
        .to_string()
}

fn root_source_path(relative: &str) -> String {
    if relative.is_empty() {
        ".".into()
    } else {
        relative.to_string()
    }
}

fn child_uri(root: &str, relative: &str) -> String {
    if relative.is_empty() {
        root.trim_end_matches('/').to_string()
    } else {
        format!("{}/{}", root.trim_end_matches('/'), relative)
    }
}

fn is_nested_in_any<'a>(path: &str, roots: impl Iterator<Item = &'a String>) -> bool {
    roots
        .into_iter()
        .any(|root| root.is_empty() || path == root || path.starts_with(&format!("{root}/")))
}

fn catalog_snapshot_id(datasets: &[CatalogDataset]) -> String {
    let mut hasher = blake3::Hasher::new();
    for dataset in datasets {
        hasher.update(dataset.mount.name.as_bytes());
        hasher.update(b"\0");
        hasher.update(dataset.mount.uri.as_bytes());
        if let Some(format) = dataset.mount.format_hint {
            hasher.update(b"\0format:");
            hasher.update(format.as_str().as_bytes());
        }
        for source in &dataset.sources {
            hasher.update(b"\0");
            hasher.update(source.file.as_bytes());
            if let Some(snapshot_ref) = &source.snapshot_ref {
                hasher.update(b"\0");
                hasher.update(snapshot_ref.as_bytes());
            }
            if let Some(error) = &source.error {
                hasher.update(b"\0");
                hasher.update(error.as_bytes());
            }
        }
    }
    hasher.finalize().to_hex()[..24].to_string()
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn redact_error(message: &str) -> String {
    // Object-store credentials should not normally occur in URLs, but avoid
    // reflecting query strings from backend errors into the public catalog.
    message
        .split_whitespace()
        .map(|part| part.split('?').next().unwrap_or(part))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ChronicleQueryEngine, EventIdentity, RawEventLanceStore, StoryCoords, StorylineAgent,
        StorylineLanceStore, StorylineTurn, StructuredStore, STORYLINE_SCHEMA_VERSION,
    };
    use object_store::ObjectStoreExt;

    fn write_openai_source(path: &Path, event_id: &str) -> Result<()> {
        fs::write(
            path,
            format!(
                r#"[{{"id":"{event_id}","session_id":"shared-session","step_id":1,"agent_model":"model","messages":[{{"role":"user","content":"hello"}}],"response":{{"role":"assistant","content":"world"}}}}]"#
            ),
        )?;
        Ok(())
    }

    fn storyline(session_id: &str, run_id: &str) -> StorylineDocument {
        StorylineDocument {
            schema_version: STORYLINE_SCHEMA_VERSION.into(),
            run_id: Some(run_id.into()),
            session_id: session_id.into(),
            agent: StorylineAgent {
                id: "agent".into(),
                name: None,
                version: None,
                model_name: Some("model".into()),
                tool_definitions: None,
                extra: None,
            },
            parent: None,
            child_session_ids: None,
            notes: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            turns: vec![StorylineTurn {
                id: 1,
                kind: None,
                timestamp: None,
                source: "user".into(),
                message: serde_json::json!("hello"),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
            }],
        }
    }

    #[test]
    fn dataset_names_are_normalized_and_validated() {
        assert_eq!(
            DatasetMount::new("DataSet", "/tmp/x").unwrap().name,
            "dataset"
        );
        assert!(DatasetMount::new("bad-name", "/tmp/x").is_err());
        assert!(DatasetMount::new("public", "/tmp/x").is_err());
    }

    #[tokio::test]
    async fn discovers_mixed_local_files_and_exposes_sources() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join("nested"))?;
        fs::write(
            temp.path().join("openai.json"),
            r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
        )?;
        fs::write(
            temp.path().join("nested/atif.jsonl"),
            r#"{"schema_version":"ATIF-v1.4","session_id":"s2","steps":[],"agent":{"id":"a"}}"#,
        )?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        assert_eq!(snapshot.datasets()[0].ready_source_count(), 2);
        assert_eq!(snapshot.datasets()[0].sources[0].file, "nested/atif.jsonl");

        let context = SessionContext::new();
        snapshot.register(&context).await?;
        let rows = context
            .sql("SELECT _file_, format FROM dataset.sources ORDER BY _file_")
            .await?
            .collect()
            .await?;
        assert_eq!(rows.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
        let compatibility_rows = context
            .sql("SELECT _file_ FROM sources ORDER BY _file_")
            .await?
            .collect()
            .await?;
        assert_eq!(
            compatibility_rows
                .iter()
                .map(RecordBatch::num_rows)
                .sum::<usize>(),
            2
        );
        Ok(())
    }

    #[tokio::test]
    async fn ignores_derived_lance_sidecars_during_discovery() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path().join("run/judgments.lance/_versions"))?;
        fs::write(
            temp.path()
                .join("run/judgments.lance/_versions/latest_version_hint.json"),
            "{}",
        )?;
        write_openai_source(&temp.path().join("trajectory.json"), "event-1")?;

        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        assert_eq!(snapshot.datasets()[0].sources.len(), 1);
        assert_eq!(snapshot.datasets()[0].sources[0].file, "trajectory.json");
        Ok(())
    }

    #[tokio::test]
    async fn report_mode_keeps_late_local_format_errors_lazy() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("broken.json"), "{")?;
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(temp.path().to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions {
                    error_policy: CatalogErrorPolicy::Report,
                    ..CatalogSnapshotOptions::default()
                },
            )
            .await?,
        );
        assert_eq!(snapshot.datasets()[0].ready_source_count(), 1);
        assert_eq!(snapshot.datasets()[0].error_source_count(), 0);
        assert_eq!(snapshot.datasets()[0].sources[0].format, None);
        assert_eq!(
            snapshot.prepared[0].sources[0]
                .resolution_count
                .load(Ordering::Relaxed),
            0
        );

        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
        let error = engine
            .query("SELECT run_id FROM dataset.runs WHERE _file_ = 'broken.json'")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("broken.json"));
        assert_eq!(
            snapshot.prepared[0].sources[0]
                .resolution_count
                .load(Ordering::Relaxed),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn empty_dataset_still_exposes_the_stable_catalog_tables() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(temp.path().to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        assert_eq!(snapshot.datasets()[0].sources.len(), 0);
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot).await?;
        let output = engine
            .query_jsonl("SELECT COUNT(*) AS runs FROM runs")
            .await?;
        assert_eq!(output.trim(), r#"{"runs":0}"#);
        Ok(())
    }

    #[tokio::test]
    async fn catalog_prunes_file_sources_before_lazy_resolution() -> Result<()> {
        let temp = tempfile::tempdir()?;
        write_openai_source(&temp.path().join("one.json"), "event-1")?;
        fs::write(temp.path().join("two.json"), "{")?;
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(temp.path().to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        assert!(snapshot.prepared[0]
            .sources
            .iter()
            .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));

        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
        assert!(snapshot.prepared[0]
            .sources
            .iter()
            .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
        assert_eq!(
            engine
                .query_jsonl("SELECT COUNT(*) AS sources FROM dataset.sources")
                .await?
                .trim(),
            r#"{"sources":2}"#
        );
        assert!(snapshot.prepared[0]
            .sources
            .iter()
            .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
        let unsafe_mixed_join = engine
            .query(
                "SELECT * FROM runs r JOIN dataset.steps s \
                 ON r.run_id = s.run_id",
            )
            .await
            .unwrap_err();
        assert!(format!("{unsafe_mixed_join:#}").contains("must include"));
        let rows = engine
            .query_jsonl(
                "SELECT run_id, step_count FROM dataset.trajectories \
                 WHERE _file_ LIKE 'one.%' AND run_id = 'shared-session'",
            )
            .await?;
        assert_eq!(rows.trim(), r#"{"run_id":"shared-session","step_count":2}"#);
        assert_eq!(
            snapshot.prepared[0]
                .sources
                .iter()
                .map(|source| source.resolution_count.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        let explain = engine
            .query_jsonl("EXPLAIN SELECT run_id FROM dataset.runs WHERE _file_ = 'one.json'")
            .await?;
        assert!(!explain.contains("UnionExec"));
        assert_eq!(engine.local_file_metrics().unwrap().files_parsed, 1);
        let error = engine
            .query("SELECT run_id FROM dataset.runs WHERE _file_ = 'two.json'")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("two.json"));
        assert_eq!(
            snapshot.prepared[0]
                .sources
                .iter()
                .map(|source| source.resolution_count.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
        Ok(())
    }

    #[tokio::test]
    async fn catalog_downloads_only_selected_remote_file_source() -> Result<()> {
        let uri = format!(
            "shared-memory://pchronicle-catalog-lazy-{}/root",
            uuid::Uuid::new_v4().simple()
        );
        let (store, root) = LanceObjectStore::from_uri(&uri).await?;
        for (file, content) in [
            (
                "one.json",
                r#"[{"id":"event-1","session_id":"shared-session","step_id":1,"agent_model":"model","messages":[],"response":{"role":"assistant","content":"world"}}]"#,
            ),
            ("two.json", "{"),
        ] {
            store
                .inner
                .put(&root.clone().join(file), content.to_string().into())
                .await?;
        }
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(uri)?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions {
                    error_policy: CatalogErrorPolicy::Report,
                    ..CatalogSnapshotOptions::default()
                },
            )
            .await?,
        );
        assert!(snapshot.prepared[0]
            .sources
            .iter()
            .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
        let rows = engine
            .query_jsonl("SELECT run_id FROM dataset.runs WHERE _file_ = 'one.json'")
            .await?;
        assert_eq!(rows.trim(), r#"{"run_id":"shared-session"}"#);
        assert_eq!(
            snapshot.prepared[0]
                .sources
                .iter()
                .map(|source| source.resolution_count.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        let error = engine
            .query("SELECT run_id FROM dataset.runs WHERE _file_ = 'two.json'")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("two.json"));
        assert_eq!(
            snapshot.prepared[0]
                .sources
                .iter()
                .map(|source| source.resolution_count.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
            vec![1, 1]
        );
        Ok(())
    }

    #[tokio::test]
    async fn catalog_prunes_storyline_sources_before_opening_lance() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for (name, session_id) in [("a", "session-a"), ("b", "session-b")] {
            let store = StorylineLanceStore::open(temp.path().join(name)).await?;
            store
                .replace_storyline(&storyline(session_id, &format!("run-{name}")))
                .await?;
        }
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(temp.path().to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        // Lazy resolution must open the generation pinned above, not follow a
        // newer CURRENT pointer published before the first query.
        StorylineLanceStore::open(temp.path().join("a"))
            .await?
            .replace_storyline(&storyline("session-a-new", "run-a-new"))
            .await?;
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
        assert!(snapshot.prepared[0]
            .sources
            .iter()
            .all(|source| source.resolution_count.load(Ordering::Relaxed) == 0));

        let rows = engine
            .query_jsonl(
                "SELECT run_id FROM dataset.runs \
                 WHERE _file_ = 'a' AND run_id = 'run-a'",
            )
            .await?;
        assert_eq!(rows.trim(), r#"{"run_id":"run-a"}"#);
        assert_eq!(
            snapshot.prepared[0]
                .sources
                .iter()
                .map(|source| source.resolution_count.load(Ordering::Relaxed))
                .collect::<Vec<_>>(),
            vec![1, 0]
        );
        Ok(())
    }

    #[tokio::test]
    async fn trajectory_bundle_derives_events_from_one_storyline_source_resolution() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let expected = storyline("bundle-session", "bundle-run");
        StorylineLanceStore::open(temp.path())
            .await?
            .replace_storyline(&expected)
            .await?;
        let snapshot = DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(temp.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?;
        let key = CatalogStorylineKey {
            dataset: DEFAULT_DATASET_NAME.into(),
            file: ".".into(),
            session_id: expected.session_id.clone(),
        };

        let bundle = snapshot
            .load_trajectory_bundle(&key)
            .await?
            .context("trajectory bundle must exist")?;

        assert_eq!(bundle.storyline, expected);
        assert_eq!(bundle.events, storyline_to_events(&bundle.storyline)?);
        assert_eq!(
            snapshot.prepared[0].sources[0]
                .resolution_count
                .load(Ordering::Relaxed),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn one_source_keeps_storylines_with_a_shared_run_id_independent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = StorylineLanceStore::open(temp.path()).await?;
        store
            .replace_storylines(&[
                storyline("root-session", "shared-run"),
                storyline("child-session", "shared-run"),
            ])
            .await?;
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(temp.path().to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;

        let rows = engine
            .query_jsonl(
                "SELECT session_id, run_id, step_count FROM dataset.trajectories \
                 ORDER BY session_id",
            )
            .await?;
        assert_eq!(
            rows.lines()
                .map(serde_json::from_str::<serde_json::Value>)
                .collect::<serde_json::Result<Vec<_>>>()?,
            vec![
                serde_json::json!({
                    "session_id": "child-session",
                    "run_id": "shared-run",
                    "step_count": 1
                }),
                serde_json::json!({
                    "session_id": "root-session",
                    "run_id": "shared-run",
                    "step_count": 1
                }),
            ]
        );

        for session_id in ["root-session", "child-session"] {
            let story = snapshot
                .load_storyline(&CatalogStorylineKey {
                    dataset: DEFAULT_DATASET_NAME.into(),
                    file: ".".into(),
                    session_id: session_id.into(),
                })
                .await?
                .context("Catalog Storyline must resolve by session_id")?;
            assert_eq!(story.session_id, session_id);
            assert_eq!(story.run_id.as_deref(), Some("shared-run"));
            assert_eq!(story.turns.len(), 1);
        }
        Ok(())
    }

    #[tokio::test]
    async fn catalog_joins_require_file_keys_only_within_one_dataset() -> Result<()> {
        let left = tempfile::tempdir()?;
        let right = tempfile::tempdir()?;
        for root in [left.path(), right.path()] {
            write_openai_source(&root.join("one.json"), "event-1")?;
            write_openai_source(&root.join("two.json"), "event-2")?;
        }
        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![
                    DatasetMount::new("left_data", left.path().to_string_lossy())?,
                    DatasetMount::new("right_data", right.path().to_string_lossy())?,
                ],
                None,
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot).await?;

        let unsafe_join = engine
            .query(
                "SELECT * FROM left_data.runs r JOIN left_data.steps s \
                 ON r.run_id = s.run_id",
            )
            .await
            .unwrap_err();
        assert!(format!("{unsafe_join:#}").contains("must include"));

        let cross_dataset = engine
            .query(
                "SELECT count(*) FROM left_data.runs l JOIN right_data.runs r \
                 ON l.run_id = r.run_id",
            )
            .await?;
        assert_eq!(
            cross_dataset
                .iter()
                .map(RecordBatch::num_rows)
                .sum::<usize>(),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_event_source_exposes_and_loads_each_storyline_independently() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let storage = temp.path().join("capture");
        for session_id in ["root", "child"] {
            let coords = StoryCoords::new(
                storage.to_string_lossy(),
                "agent",
                session_id,
                Some("run-1".into()),
            );
            RawEventLanceStore
                .append_events(
                    &coords,
                    &[EventRecord {
                        identity: EventIdentity::default(),
                        seq: 0,
                        source: "test".into(),
                        kind: "note".into(),
                        timestamp: None,
                        session_id: Some(session_id.into()),
                        agent_id: Some("agent".into()),
                        parent_uuid: None,
                        trace_id: None,
                        call_id: None,
                        subagent_id: None,
                        parent_agent_id: None,
                        branch: None,
                        parent_call_id: None,
                        payload: serde_json::json!({"session": session_id}),
                    }],
                )
                .await?;
        }

        let snapshot = Arc::new(
            DatasetCatalogSnapshot::discover(
                vec![DatasetMount::default(storage.to_string_lossy())?],
                Some(DEFAULT_DATASET_NAME.into()),
                CatalogSnapshotOptions::default(),
            )
            .await?,
        );
        assert_eq!(snapshot.datasets()[0].sources.len(), 1);
        assert_eq!(
            snapshot.datasets()[0].sources[0].file,
            "agent/run-1/events.lance"
        );
        let updated_coords = StoryCoords::new(
            storage.to_string_lossy(),
            "agent",
            "root",
            Some("run-1".into()),
        );
        RawEventLanceStore
            .append_events(
                &updated_coords,
                &[EventRecord {
                    identity: EventIdentity::default(),
                    seq: 1,
                    source: "test".into(),
                    kind: "note".into(),
                    timestamp: None,
                    session_id: Some("root".into()),
                    agent_id: Some("agent".into()),
                    parent_uuid: None,
                    trace_id: None,
                    call_id: None,
                    subagent_id: None,
                    parent_agent_id: None,
                    branch: None,
                    parent_call_id: None,
                    payload: serde_json::json!({"after": "snapshot"}),
                }],
            )
            .await?;
        let lazy = &snapshot.prepared[0].sources[0];
        assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 0);
        let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot.clone()).await?;
        assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 0);
        let event_count = engine
            .query_jsonl(
                "SELECT COUNT(*) AS rows FROM dataset.events \
                 WHERE _file_ = 'agent/run-1/events.lance' AND seq = 0",
            )
            .await?;
        assert_eq!(event_count.trim(), r#"{"rows":2}"#);
        assert_eq!(lazy.resolution_count.load(Ordering::Relaxed), 1);
        let resolved = lazy.resolved.get().unwrap().as_ref().unwrap();
        let ResolvedSource::Events(events) = resolved.as_ref() else {
            panic!("canonical event source resolved to the wrong adapter");
        };
        assert_eq!(events.normalization_count.load(Ordering::Relaxed), 0);
        let runs = engine
            .query_jsonl("SELECT _file_, run_id, session_id FROM dataset.runs ORDER BY session_id")
            .await?;
        assert_eq!(events.normalization_count.load(Ordering::Relaxed), 1);
        let keys = runs
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<serde_json::Result<Vec<_>>>()?;
        assert_eq!(keys.len(), 2);
        for row in keys {
            let events = snapshot
                .load_events(&CatalogStorylineKey {
                    dataset: DEFAULT_DATASET_NAME.into(),
                    file: row["_file_"].as_str().unwrap().into(),
                    session_id: row["session_id"].as_str().unwrap().into(),
                })
                .await?
                .context("Catalog Storyline must resolve canonical events")?;
            assert_eq!(events.events.len(), 1);
        }
        Ok(())
    }
}
