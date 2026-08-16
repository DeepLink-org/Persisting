//! Query-time, immutable catalogs over one or more trajectory dataset mounts.
//!
//! A catalog is deliberately not a second durable metadata store. Discovery
//! freezes the source membership and version descriptors seen by one
//! query/Web snapshot. Physical sources are opened lazily after catalog-aware
//! `_file_` pruning selects them.

mod discovery;

use discovery::{
    bind_canonical_storyline_projections, discover_candidates, freeze_candidate,
    normalize_event_source,
};

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
    projection_lineage_is_fresh, reconstruct_storyline, split_storyline, ChronicleFormat,
    EventRecord, EventsDocument, ProjectionSourceSnapshot, StoryRunRow, StoryStepRow,
    StoryToolCallRow, StorylineDocument,
};

use super::events::datafusion::{RawEventDataSourceOptions, RawEventSnapshot};
use super::files::matches_file_filter;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogProjectionStatus {
    Fresh,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredSource {
    /// Stable path relative to the Dataset mount. A source at the mount root
    /// is represented by `.`.
    pub file: String,
    pub format: Option<String>,
    pub kind: CatalogSourceKind,
    pub snapshot_ref: Option<String>,
    pub projection_status: Option<CatalogProjectionStatus>,
    pub projection_generation: Option<String>,
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
            bind_canonical_storyline_projections(&mut source_rows, &mut prepared_sources)?;
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
        projection: Option<StorylineTablePaths>,
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
            LazySourceSpec::Events {
                snapshot,
                projection,
                ..
            } => {
                let source = RawEventDataSource::from_pinned_snapshot_with_options(
                    snapshot.clone(),
                    RawEventDataSourceOptions::default(),
                )
                .await?;
                let projection = match projection {
                    Some(paths) => Some(
                        StorylineDataSource::from_pinned_paths_with_options(
                            paths.clone(),
                            self.options.storyline,
                        )
                        .await?,
                    ),
                    None => None,
                };
                Ok(ResolvedSource::Events(ResolvedEventSource {
                    source,
                    projection,
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
    projection: Option<StorylineDataSource>,
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
                if let Some(projection) = &source.projection {
                    return Ok(storyline_kind().map(|kind| ResolvedTable {
                        provider: projection.provider(kind),
                        carries_file_column: false,
                    }));
                }
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
            if let Some(projection) = &source.projection {
                return projection.register(context);
            }
            let normalized = source.normalized().await?;
            context.register_table("runs", normalized.runs.clone())?;
            context.register_table("steps", normalized.steps.clone())?;
            context.register_table("tool_calls", normalized.tool_calls.clone())?;
            Ok(())
        }
    }
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
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| {
                        source.projection_status.map(|status| match status {
                            CatalogProjectionStatus::Fresh => "fresh",
                            CatalogProjectionStatus::Stale => "stale",
                        })
                    })
                    .collect::<Vec<_>>(),
            )),
            Arc::new(StringArray::from(
                sources
                    .iter()
                    .map(|source| source.projection_generation.as_deref())
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
        Field::new("projection_status", DataType::Utf8, true),
        Field::new("projection_generation", DataType::Utf8, true),
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
            if let Some(projection_generation) = &source.projection_generation {
                hasher.update(b"\0projection:");
                hasher.update(projection_generation.as_bytes());
                hasher.update(match source.projection_status {
                    Some(CatalogProjectionStatus::Fresh) => b":fresh",
                    Some(CatalogProjectionStatus::Stale) => b":stale",
                    None => b":none",
                });
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
mod tests;
