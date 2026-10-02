//! pChronicle 的持久化存储入口。

pub type Result<T> = anyhow::Result<T>;
#[cfg(feature = "lance-store")]
pub use crate::store::opendal_store::StoreConfig;
#[cfg(feature = "lance-store")]
pub use crate::store::opendal_store::{RetryPatience, set_retry_patience};

/// Parse an integer byte size with binary IEC suffixes.
pub fn parse_byte_size(value: &str) -> std::result::Result<usize, String> {
    let value = value.trim();
    let suffixes = [
        ("KiB", 1024usize),
        ("MiB", 1024 * 1024),
        ("GiB", 1024 * 1024 * 1024),
    ];
    let (number, multiplier) = suffixes
        .iter()
        .find_map(|(suffix, multiplier)| {
            value
                .strip_suffix(suffix)
                .map(|number| (number, *multiplier))
        })
        .unwrap_or((value, 1));
    let amount = number
        .parse::<usize>()
        .map_err(|_| format!("invalid byte size '{value}'; use an integer or KiB, MiB, GiB"))?;
    amount
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| "byte size must be greater than zero and fit in usize".to_owned())
}

pub use crate::agenticmd::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, sanitize_session_filename,
    session_filename_stem, session_markdown_filename, session_markdown_path_for_key,
    session_markdown_write_path_for_key,
};

#[cfg(feature = "lance-store")]
pub use crate::store::blockcache::{
    BlockCache, CacheConfig, CacheStats, CachedObjectStore, DEFAULT_BLOCK_SIZE_BYTES,
    DEFAULT_CAPACITY_BYTES, LanceCacheWrapper, SERVE_CAPACITY_BYTES, capacity_for_serve,
    configured_capacity_bytes, default_cache_dir, lance_store_params,
};
#[cfg(feature = "lance-store")]
pub use crate::store::index_build_progress::{
    Guard as IndexBuildProgressGuard, install as install_index_build_progress,
};
#[cfg(feature = "lance-store")]
pub use crate::store::object_store_io_gate::{
    IoKind as ObjectStoreIoKind, ObjectStoreGateSnapshot, ObjectStoreThrottleEvent,
    ObjectStoreThrottleHookGuard, foreground_object_store_demand,
    format_aimd_flow_label as format_object_store_aimd_flow_label,
    install_throttle_hook as install_object_store_throttle_hook,
    snapshot as object_store_gate_snapshot, wait_for_foreground_object_store_idle,
    with_background_object_store_io,
};

#[cfg(feature = "lance-store")]
pub async fn open_lance_dataset(uri: &str) -> lance::Result<lance::Dataset> {
    lance::dataset::builder::DatasetBuilder::from_uri(uri)
        .with_store_params(crate::store::blockcache::lance_store_params(
            crate::store::blockcache::configured_capacity_bytes(),
        ))
        .load()
        .await
}

#[cfg(feature = "lance-store")]
pub use crate::store::{
    CachedDataset, CatalogConsistency, CatalogDataset, CatalogErrorPolicy, CatalogNamespace,
    CatalogPage, CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind,
    CatalogSourceRevision, CatalogSourceStatus, CatalogState, CatalogStatus, CatalogStorylineKey,
    ChronicleManifest, CompactJsonlBuildPhase, CompactJsonlColumn, CompactJsonlImportEvent,
    CompactJsonlOffload, CompactJsonlOptions, CompactJsonlRecord, CompactJsonlStore,
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_DATASET_NAME,
    DEFAULT_MAX_CHUNK_BYTES, DEFAULT_PHYSICAL_PAGE_LIMIT, Dataset, DatasetCatalogSnapshot,
    DatasetLocation, DatasetLocationKind, DatasetMount, DatasetResolver, DiscoveredSource,
    ImportableObjectEvent, LanceMaintenanceOptions, LanceMaintenanceReport, LocationSummary,
    ManifestCache, ManifestKind, ManifestListing, ManifestReadMode, ManifestRefreshReport,
    ManifestStats, NamespacePath, PathListEntry, PathListKind, PersistentCache, PhysicalColumn,
    PhysicalDataFile, PhysicalFileLayout, PhysicalFragment, PhysicalLayout, PhysicalPage,
    PhysicalPagePreview, PhysicalPageQuery, PhysicalSource, PhysicalTable, QueryScope, ResolveMode,
    ResolveTarget, ShallowNavEntry, StorylineContentOptions, StorylineContentReadMode,
    StorylineDataSource, StorylineDataSourceOptions, StorylineLanceStore,
    StorylineMaintenanceReport, StorylineSearchIndexSuppressGuard, StorylineStreamImportReport,
    StorylineStreamOptions, StorylineTablePaths, inspect_physical_file, inspect_physical_layout,
    inspect_physical_page, list_physical_sources, load_manifest, load_manifest_at_uri,
    write_compact_jsonl_manifest, write_storyline_manifest, write_storyline_manifest_at_uri,
};

// Compatibility exports; new callers should use `crate::search`.
#[cfg(feature = "lance-store")]
pub use crate::search::{
    search_storyline_documents_fts, search_storyline_step_matches_fts,
    search_storyline_step_matches_fts_in_columns, search_storyline_steps_fts,
    storyline_steps_fts_available,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES};
