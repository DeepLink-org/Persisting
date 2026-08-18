//! Query-time, immutable catalogs over one or more trajectory dataset mounts.
//!
//! A catalog is deliberately not a second durable metadata store. Discovery
//! freezes the source membership and version descriptors seen by one
//! query/Web snapshot. Physical sources are opened lazily after catalog-aware
//! `_file_` pruning selects them.

mod discovery;
mod identity;
mod namespace;
mod provider;
mod source;

pub use identity::{CatalogSourceRevision, DatasetMount, NamespacePath};
pub use namespace::{CatalogNamespace, CatalogPage, CatalogSourceDescription};
use provider::*;
use source::*;

use discovery::{
    bind_canonical_storyline_projections, discover_candidates, freeze_candidate,
    normalize_event_storylines,
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
use futures::{stream, StreamExt, TryStreamExt};
use lance::io::ObjectStore as LanceObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectMeta};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

use crate::convert::{event_storyline_key, project_event_records, storyline_to_events};
use crate::format::DocumentFormat;
use crate::formats::events::EventsDocument;
use crate::formats::{EventRecord, StorylineDocument};
use crate::projection::projection_lineage_is_fresh;

use super::events::datafusion::{RawEventDataSourceOptions, RawEventSnapshot};
use super::files::matches_file_filter;
use super::{
    raw_event_arrow_schema, reconstruct_storyline, split_storyline, story_runs_arrow_schema,
    story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema, story_steps_from_batch,
    story_steps_to_batch, story_tool_calls_arrow_schema, story_tool_calls_from_batch,
    story_tool_calls_to_batch, FileTrajectoryDataSource, FileTrajectoryDataSourceOptions,
    FileTrajectoryQueryMetrics, LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions,
    ProjectionSourceSnapshot, RawEventDataSource, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineDataSource, StorylineDataSourceOptions, StorylineTableKind, StorylineTablePaths,
    SOURCE_FILE_COLUMN,
};

pub const DEFAULT_DATASET_NAME: &str = "dataset";
pub const CATALOG_SOURCES_TABLE: &str = "sources";
pub const CATALOG_TRAJECTORIES_TABLE: &str = "trajectories";
pub const DEFAULT_MAX_EVENT_FALLBACK_ROWS: usize = 100_000;
pub const DEFAULT_MAX_EVENT_FALLBACK_BYTES: usize = 64 * 1024 * 1024;

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
    pub revision: Option<CatalogSourceRevision>,
    pub projection_status: Option<CatalogProjectionStatus>,
    pub projection_generation: Option<String>,
    /// Number of lineage-linked Storyline projections considered for this
    /// canonical source. Values greater than one are diagnostic, not fatal.
    pub projection_candidates: u64,
    pub size_bytes: Option<u64>,
    pub last_modified: Option<String>,
    pub status: CatalogSourceStatus,
    pub error: Option<String>,
}

impl DiscoveredSource {
    pub fn snapshot_ref(&self) -> Option<String> {
        self.revision
            .as_ref()
            .map(CatalogSourceRevision::snapshot_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogStorylineKey {
    pub dataset: String,
    pub file: String,
    /// Stable identity of one document within `file`.
    pub document_id: String,
    /// Session partition used when the source is Canonical Event storage.
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
    pub(crate) manifest: LocalQueryManifestOptions,
    pub(crate) files: FileTrajectoryDataSourceOptions,
    pub(crate) storyline: StorylineDataSourceOptions,
    /// Maximum physical Sources opened concurrently while planning one scan.
    pub max_concurrent_sources: usize,
    /// Maximum selected canonical rows that may be normalized in memory when a
    /// fresh Storyline projection is unavailable.
    pub max_event_fallback_rows: usize,
    /// Maximum Arrow bytes retained while normalizing selected canonical rows.
    pub max_event_fallback_bytes: usize,
}

impl CatalogSnapshotOptions {
    /// Configure bounded source discovery without exposing provider manifests.
    pub fn with_discovery_limits(mut self, max_files: usize, max_entries: usize) -> Self {
        self.manifest.max_files = max_files;
        self.manifest.max_entries = max_entries;
        self
    }

    pub fn with_error_policy(mut self, error_policy: CatalogErrorPolicy) -> Self {
        self.error_policy = error_policy;
        self
    }
}

impl Default for CatalogSnapshotOptions {
    fn default() -> Self {
        let max_concurrent_sources = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(16);
        Self {
            error_policy: CatalogErrorPolicy::Strict,
            manifest: LocalQueryManifestOptions::default(),
            files: FileTrajectoryDataSourceOptions::default(),
            storyline: StorylineDataSourceOptions::default(),
            max_concurrent_sources,
            max_event_fallback_rows: DEFAULT_MAX_EVENT_FALLBACK_ROWS,
            max_event_fallback_bytes: DEFAULT_MAX_EVENT_FALLBACK_BYTES,
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
    /// Build a read-only query engine over this catalog snapshot.
    pub async fn query_engine(
        self: Arc<Self>,
        options: super::ChronicleQueryExecutionOptions,
    ) -> Result<super::ChronicleQueryEngine> {
        super::ChronicleQueryEngine::from_catalog_snapshot_with_options(self, options).await
    }

    pub async fn discover(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
    ) -> Result<Self> {
        anyhow::ensure!(!mounts.is_empty(), "mount at least one Dataset");
        validate_catalog_options(options)?;

        let mut names = HashSet::with_capacity(mounts.len());
        let mut namespaces = HashSet::with_capacity(mounts.len());
        for mount in &mounts {
            anyhow::ensure!(
                names.insert(mount.name.clone()),
                "duplicate Dataset name '{}'",
                mount.name
            );
            anyhow::ensure!(
                namespaces.insert(mount.namespace.clone()),
                "duplicate Namespace '{}'",
                mount.namespace.display_name()
            );
        }
        let default_dataset = default_dataset
            .map(|name| identity::normalize_sql_alias(&name))
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
                max_concurrent_sources: options.max_concurrent_sources,
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
            let Some(records) = events.records_for_storyline(&key.session_id).await? else {
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
            return Ok(source
                .records_for_storyline(&key.session_id)
                .await?
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
        let dataset = identity::normalize_sql_alias(&key.dataset)?;
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
                let provider: Arc<dyn TableProvider> = Arc::new(CatalogTableProvider::new(
                    prepared.sources.clone(),
                    kind,
                    prepared.max_concurrent_sources,
                ));
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
    anyhow::ensure!(
        options.max_concurrent_sources > 0,
        "catalog max_concurrent_sources must be positive"
    );
    anyhow::ensure!(
        options.max_event_fallback_rows > 0,
        "catalog max_event_fallback_rows must be positive"
    );
    anyhow::ensure!(
        options.max_event_fallback_bytes > 0,
        "catalog max_event_fallback_bytes must be positive"
    );
    Ok(())
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

fn remote_source_revision(meta: &ObjectMeta) -> CatalogSourceRevision {
    CatalogSourceRevision::Object {
        version: meta.version.clone(),
        etag: meta.e_tag.clone(),
        size_bytes: meta.size,
        last_modified: meta.last_modified.to_rfc3339(),
        location: meta.location.to_string(),
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
            if let Some(snapshot_ref) = source.snapshot_ref() {
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
