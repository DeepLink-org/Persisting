//! pChronicle storage, separated by logical data model.
//!
//! - `storyline`: normalized three-table storage for `StorylineDocument`.
//! - `search`: document retrieval storage lives outside this module.

#[cfg(feature = "lance-store")]
mod agenticmd_datafusion;
#[cfg(feature = "lance-store")]
pub(crate) mod blockcache;
#[cfg(feature = "lance-store")]
mod catalog;
#[cfg(feature = "lance-store")]
mod compact_jsonl;
#[cfg(feature = "lance-store")]
mod datafusion_bridge;
#[cfg(feature = "lance-store")]
mod document_source;
#[cfg(feature = "lance-store")]
mod files;
#[cfg(feature = "lance-store")]
pub(crate) mod index_build_gate;
#[cfg(feature = "lance-store")]
pub(crate) mod index_build_progress;
#[cfg(feature = "lance-store")]
mod inspect;
#[cfg(feature = "lance-store")]
mod local_query_manifest;
#[cfg(feature = "lance-store")]
pub(crate) mod object_store_io_gate;
#[cfg(feature = "lance-store")]
pub(crate) mod opendal_store;
#[cfg(feature = "lance-store")]
pub mod persistent_cache;
#[cfg(feature = "lance-store")]
mod query_engine;
#[cfg(feature = "lance-store")]
pub use persistent_cache::PersistentCache;
#[cfg(feature = "lance-store")]
mod root_write_lock;
#[cfg(feature = "lance-store")]
mod storyline;
#[cfg(feature = "lance-store")]
#[path = "storyline/model.rs"]
mod storyline_model;
#[cfg(feature = "lance-store")]
mod virtual_document;

#[cfg(feature = "lance-store")]
pub(crate) use agenticmd_datafusion::AgenticMdDataSource;
#[cfg(feature = "lance-store")]
pub use catalog::location::{
    DatasetLocation, DatasetLocationKind, ImportableObjectEvent, PathListEntry, PathListKind,
    ShallowNavEntry,
};
#[cfg(feature = "lance-store")]
#[allow(unused_imports)]
pub use catalog::manifest::{
    CHRONICLE_MANIFEST_FILE, ChronicleManifest, ManifestKind, ManifestStats, STORYLINE_FORMAT,
    atomic_write_manifest, compact_jsonl_manifest_matches, load_manifest, load_manifest_at_uri,
    try_load_manifest, write_compact_jsonl_manifest, write_storyline_manifest,
    write_storyline_manifest_at_uri,
};
#[cfg(feature = "lance-store")]
pub use catalog::{
    CATALOG_SOURCES_TABLE, CATALOG_TRAJECTORIES_TABLE, CachedDataset, CatalogConsistency,
    CatalogDataset, CatalogErrorPolicy, CatalogNamespace, CatalogPage, CatalogSnapshotOptions,
    CatalogSourceDescription, CatalogSourceKind, CatalogSourceRevision, CatalogSourceStatus,
    CatalogState, CatalogStatus, CatalogStorylineKey, DEFAULT_DATASET_NAME, Dataset,
    DatasetCatalogSnapshot, DatasetMount, DatasetResolver, DiscoveredSource, LocationSummary,
    ManifestCache, ManifestListing, ManifestReadMode, ManifestRefreshReport, NamespacePath,
    QueryScope, ResolveMode, ResolveTarget,
};
#[cfg(feature = "lance-store")]
pub use compact_jsonl::{
    CompactJsonlBuildPhase, CompactJsonlColumn, CompactJsonlImportEvent, CompactJsonlOffload,
    CompactJsonlOptions, CompactJsonlRecord, CompactJsonlStore,
};
#[cfg(feature = "lance-store")]
pub(crate) use document_source::{DocumentSourceImpl, open_document_source};
#[cfg(feature = "lance-store")]
pub(crate) use files::{
    AtifReader, FileTrajectoryDataSource, FileTrajectoryDataSourceOptions,
    FileTrajectoryQueryMetrics,
};
#[cfg(feature = "lance-store")]
pub use files::{FileTrajectoryQueryMetricsSnapshot, SOURCE_FILE_COLUMN};
#[cfg(feature = "lance-store")]
pub use inspect::{
    DEFAULT_PHYSICAL_PAGE_LIMIT, PhysicalColumn, PhysicalDataFile, PhysicalFileLayout,
    PhysicalFragment, PhysicalLayout, PhysicalPage, PhysicalPagePreview, PhysicalPageQuery,
    PhysicalSource, PhysicalTable, inspect_physical_file, inspect_physical_layout,
    inspect_physical_page, list_physical_sources,
};
#[cfg(feature = "lance-store")]
pub use local_query_manifest::{DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES};
#[cfg(feature = "lance-store")]
pub(crate) use local_query_manifest::{
    LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions,
};
#[cfg(feature = "lance-store")]
pub use query_engine::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, DEFAULT_QUERY_MEMORY_LIMIT_BYTES,
    ExternalTableFormat, ExternalTableSpec, IntrospectedField, IntrospectedTable,
    QUERY_MEMORY_LIMIT_ENV, QueryBackendInfo, QuerySnapshot, QueryWriteOutcome,
};
#[cfg(feature = "lance-store")]
pub use storyline::{
    DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE,
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_MAX_CHUNK_BYTES,
    StorylineContentOptions, StorylineContentReadMode, StorylineDataFusionTableNames,
    StorylineDataSource, StorylineDataSourceOptions, StorylineLanceStore,
    StorylineMaintenanceReport, StorylineSearchIndexSuppressGuard, StorylineStreamImportReport,
    StorylineStreamOptions, StorylineTableKind, StorylineTablePaths, story_runs_arrow_schema,
    story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema, story_steps_from_batch,
    story_steps_to_batch, story_tool_calls_arrow_schema, story_tool_calls_from_batch,
    story_tool_calls_to_batch,
};
#[cfg(feature = "lance-store")]
pub use storyline_model::{
    StoryRunRow, StoryStepRow, StoryToolCallRow, StorylineTables, reconstruct_storyline,
    split_storyline,
};

#[cfg(feature = "lance-store")]
use std::time::Duration;
#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, PartialEq)]
pub struct LanceMaintenanceOptions {
    pub compact: bool,
    pub optimize_indices: bool,
    pub vacuum_older_than: Option<Duration>,
    pub target_rows_per_fragment: usize,
    /// Maximum live source rows compacted per Storyline data table
    /// (runs/steps/tool_calls) per maintenance call. None leaves compaction
    /// unlimited. An undersized budget can prevent progress; raise it if no
    /// compaction task fits.
    pub max_compaction_source_rows: Option<usize>,
    /// Maximum source data/overlay file bytes per Storyline table per call.
    /// Excludes separate Blob v2 payloads and does not bound total memory or
    /// index/GC work. An undersized budget can leave no eligible task; raise it
    /// if compaction makes no progress. Requires recorded source file sizes.
    /// None leaves compaction unlimited.
    pub max_compaction_source_bytes: Option<u64>,
}

#[cfg(feature = "lance-store")]
impl Default for LanceMaintenanceOptions {
    fn default() -> Self {
        Self {
            compact: true,
            optimize_indices: true,
            vacuum_older_than: Some(Duration::from_secs(7 * 24 * 60 * 60)),
            target_rows_per_fragment: 1024 * 1024,
            max_compaction_source_rows: None,
            max_compaction_source_bytes: None,
        }
    }
}

#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanceMaintenanceReport {
    pub fragments_removed: usize,
    pub fragments_added: usize,
    pub old_versions_removed: u64,
    pub bytes_removed: u64,
    pub final_version: Option<u64>,
}
