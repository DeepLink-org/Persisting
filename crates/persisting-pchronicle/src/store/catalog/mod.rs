//! Query-time, immutable catalogs over one or more trajectory dataset mounts.
//!
//! A catalog is deliberately not a second durable metadata store. Discovery
//! freezes the source membership and version descriptors seen by one
//! query/Web snapshot. Physical sources are opened lazily after catalog-aware
//! `_file_` pruning selects them.

mod discovery;
mod identity;
pub mod location;
pub mod manifest;
mod manifest_cache;
mod namespace;
mod provider;
mod resolver;
mod source;
mod status;

pub use identity::{CatalogSourceRevision, DatasetMount, NamespacePath};
#[allow(unused_imports)]
pub use location::{
    DatasetLocation, DatasetLocationKind, ImportableObjectEvent, PathListEntry, PathListKind,
    ShallowNavEntry,
};
#[allow(unused_imports)]
pub use manifest::{CHRONICLE_MANIFEST_FILE, ChronicleManifest, ManifestKind, ManifestStats};
#[allow(unused_imports)]
pub use manifest::{
    STORYLINE_FORMAT, atomic_write_manifest, compact_jsonl_manifest_matches, load_manifest,
    load_manifest_at_uri, try_load_manifest, write_compact_jsonl_manifest,
    write_storyline_manifest, write_storyline_manifest_at_uri,
};
pub use manifest_cache::{
    LocationSummary, ManifestCache, ManifestListing, ManifestReadMode, ManifestRefreshReport,
};
pub use namespace::{CatalogNamespace, CatalogPage, CatalogSourceDescription};
use provider::*;
pub use resolver::{CachedDataset, Dataset, DatasetResolver, ResolveMode, ResolveTarget};
use source::*;
pub use status::{CatalogConsistency, CatalogState, CatalogStatus};

use discovery::{
    discover_cached_candidates, discover_candidate_at, discover_candidates, freeze_candidate,
};

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use datafusion::arrow::array::{Array, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::Session;
use datafusion::common::{DataFusionError, ScalarValue, TableReference};
use datafusion::datasource::{MemTable, TableProvider};
use datafusion::logical_expr::{Expr, Operator, TableProviderFilterPushDown, TableType};
use datafusion::physical_expr::expressions::{Literal, col as physical_col};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::empty::EmptyExec;
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::projection::{ProjectionExec, ProjectionExpr};
use datafusion::physical_plan::union::UnionExec;
use datafusion::prelude::SessionContext;
use futures::{StreamExt, TryStreamExt, stream};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

use crate::format::DocumentFormat;
use crate::formats::StorylineDocument;

use super::files::matches_file_filter;
use super::opendal_store::Entry as OpendalEntry;
use super::{
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryQueryMetrics,
    LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions, SOURCE_FILE_COLUMN,
    StorylineDataSource, StorylineDataSourceOptions, StorylineTableKind, StorylineTablePaths,
    reconstruct_storyline, story_runs_arrow_schema, story_runs_from_batch,
    story_steps_arrow_schema, story_steps_from_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch,
};

#[derive(Clone, Debug)]
pub(crate) struct RemoteObjectMeta {
    pub(crate) location: String,
    pub(crate) size: u64,
    pub(crate) etag: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) last_modified: String,
}

impl From<OpendalEntry> for RemoteObjectMeta {
    fn from(entry: OpendalEntry) -> Self {
        Self {
            location: entry.path,
            size: entry.metadata.content_length(),
            etag: entry.metadata.etag().map(ToOwned::to_owned),
            version: entry.metadata.version().map(ToOwned::to_owned),
            last_modified: entry
                .metadata
                .last_modified()
                .map(|value| value.to_string())
                .unwrap_or_default(),
        }
    }
}

pub const DEFAULT_DATASET_NAME: &str = "dataset";

pub const CATALOG_SOURCES_TABLE: &str = "sources";
pub const CATALOG_TRAJECTORIES_TABLE: &str = "trajectories";

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
    /// Navigational Directory child under a non-Dataset mount. Not queryable.
    Directory,
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
    pub revision: Option<CatalogSourceRevision>,
    /// Physical source size when available.
    pub size_bytes: Option<u64>,
    pub last_modified: Option<String>,
    pub status: CatalogSourceStatus,
    pub error: Option<String>,
    /// Aggregate record/run count from `chronicle.manifest` when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_count: Option<u64>,
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
    /// Session identity within the document.
    pub session_id: String,
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
            .filter(|source| {
                source.status == CatalogSourceStatus::Ready
                    && source.kind != CatalogSourceKind::Directory
            })
            .count()
    }

    pub fn directory_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| source.kind == CatalogSourceKind::Directory)
            .count()
    }

    pub fn error_source_count(&self) -> usize {
        self.sources
            .iter()
            .filter(|source| {
                source.status == CatalogSourceStatus::Error
                    && source.kind != CatalogSourceKind::Directory
            })
            .count()
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryScope {
    pub dataset: String,
    pub source_file: Option<String>,
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
        Self::discover_impl(mounts, default_dataset, options, None, None).await
    }

    pub async fn discover_scoped(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
        scope: QueryScope,
    ) -> Result<Self> {
        Self::discover_impl(mounts, default_dataset, options, Some(scope), None).await
    }

    /// Build a snapshot from source paths already observed by a UI cache.
    /// Missing or unsupported cached paths are ignored; callers that require
    /// exact membership must use [`Self::discover`] instead.
    pub async fn discover_scoped_from_cached_files(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
        scope: QueryScope,
        cached_files: Vec<String>,
    ) -> Result<Self> {
        Self::discover_impl(
            mounts,
            default_dataset,
            options,
            Some(scope),
            Some(&cached_files),
        )
        .await
    }

    async fn discover_impl(
        mounts: Vec<DatasetMount>,
        default_dataset: Option<String>,
        options: CatalogSnapshotOptions,
        scope: Option<QueryScope>,
        cached_files: Option<&[String]>,
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
            let candidates = match scope.as_ref() {
                Some(scope) if scope.dataset != mount.name => Vec::new(),
                Some(scope) => match scope.source_file.as_deref() {
                    Some(file) => discover_candidate_at(&mount, file, options.manifest).await?,
                    None => match cached_files {
                        Some(files) => {
                            discover_cached_candidates(&mount, files, options.manifest).await?
                        }
                        None => discover_candidates(&mount, options.manifest).await?,
                    },
                },
                None => discover_candidates(&mount, options.manifest).await?,
            };
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
                        source_rows.push(reported_source_failure(stub, error));
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("discover Dataset '{}' source '{}'", mount.name, stub.file)
                        });
                    }
                }
            }
            if let Some(prefix) = scope
                .as_ref()
                .and_then(|scope| scope.source_file.as_deref())
            {
                let prefix = prefix.trim().trim_matches('/');
                let matches =
                    |file: &str| file == prefix || file.starts_with(&format!("{prefix}/"));
                // Preserve the requested source namespace on fallback paths.
                source_rows.retain(|source| matches(&source.file));
                prepared_sources.retain(|source| matches(source.file()));
            }
            source_rows.sort_by(|left, right| {
                directory_sort_key(left.kind)
                    .cmp(&directory_sort_key(right.kind))
                    .then_with(|| left.file.cmp(&right.file))
            });
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

    pub async fn compact_records(
        &self,
        dataset: &str,
        file: &str,
    ) -> Result<Option<Vec<crate::store::CompactJsonlRecord>>> {
        let key = CatalogStorylineKey {
            dataset: dataset.into(),
            file: file.into(),
            document_id: String::new(),
            session_id: String::new(),
        };
        let source = self.lazy_source(&key)?;
        let LazySourceSpec::Compact { uri } = &source.spec else {
            return Ok(None);
        };
        Ok(Some(crate::store::CompactJsonlStore::records(uri).await?))
    }

    /// Page compact-jsonl identities without materializing the full source.
    pub async fn compact_records_page(
        &self,
        dataset: &str,
        file: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Option<(Vec<crate::store::CompactJsonlRecord>, u64)>> {
        let key = CatalogStorylineKey {
            dataset: dataset.into(),
            file: file.into(),
            document_id: String::new(),
            session_id: String::new(),
        };
        let source = self.lazy_source(&key)?;
        let LazySourceSpec::Compact { uri } = &source.spec else {
            return Ok(None);
        };
        Ok(Some(
            crate::store::CompactJsonlStore::records_page(uri, offset, limit).await?,
        ))
    }

    pub async fn compact_record(
        &self,
        key: &CatalogStorylineKey,
    ) -> Result<Option<serde_json::Value>> {
        let source = self.lazy_source(key)?;
        let LazySourceSpec::Compact { uri } = &source.spec else {
            return Ok(None);
        };
        crate::store::CompactJsonlStore::read_record(uri, &key.document_id).await
    }

    /// Return the normalized Storyline table paths for a physical Storyline
    /// source. Other source kinds return `None`.
    pub fn storyline_table_paths(
        &self,
        dataset: &str,
        file: &str,
    ) -> Result<Option<StorylineTablePaths>> {
        let dataset_name = identity::normalize_sql_alias(dataset)?;
        let prepared = self
            .prepared
            .iter()
            .find(|candidate| candidate.name == dataset_name)
            .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
        let source = prepared
            .sources
            .iter()
            .find(|source| source.file() == file)
            .with_context(|| format!("physical source not found: {dataset}/{file}"))?;
        Ok(match &source.spec {
            LazySourceSpec::Storyline { paths } => Some(paths.clone()),
            _ => None,
        })
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
            for format in super::virtual_document::formats() {
                let Some(table_name) = super::virtual_document::table_name(format) else {
                    continue;
                };
                let provider: Arc<dyn TableProvider> =
                    Arc::new(CatalogVirtualDocumentTableProvider::new(
                        prepared.sources.clone(),
                        format,
                        prepared.max_concurrent_sources,
                    ));
                register_catalog_provider(
                    context,
                    &dataset.mount.name,
                    table_name,
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

fn directory_sort_key(kind: CatalogSourceKind) -> u8 {
    match kind {
        CatalogSourceKind::Directory => 0,
        CatalogSourceKind::Store | CatalogSourceKind::File => 1,
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
        || path.join("_versions").is_dir()
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
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hash.update(&duration.as_nanos().to_le_bytes());
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

fn remote_source_revision(meta: &RemoteObjectMeta) -> CatalogSourceRevision {
    CatalogSourceRevision::Object {
        version: meta.version.clone(),
        etag: meta.etag.clone(),
        size_bytes: meta.size,
        last_modified: meta.last_modified.clone(),
        location: meta.location.clone(),
    }
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

fn reported_source_failure(mut source: DiscoveredSource, error: anyhow::Error) -> DiscoveredSource {
    tracing::error!(
        error = ?error,
        source = %source.file,
        "pChronicle Catalog source discovery failed"
    );
    source.status = CatalogSourceStatus::Error;
    source.error = Some("Source discovery failed".into());
    source
}

#[cfg(test)]
mod tests;
