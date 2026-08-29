//! pChronicle 的持久化存储入口。

pub type Result<T> = anyhow::Result<T>;

#[cfg(feature = "lance-store")]
pub use crate::append_queue::{
    DEFAULT_RAW_EVENT_BATCH_DELAY, DEFAULT_RAW_EVENT_BATCH_SIZE,
    DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD, DEFAULT_RAW_EVENT_HIERARCHY_FANOUT,
    DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY, DEFAULT_RAW_EVENT_QUEUE_CAPACITY,
    DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT, RawEventAppendOutcome, RawEventAppendSender,
    RawEventAppendWorker, raw_event_append_queue, raw_event_append_queue_with_capacity,
    raw_event_append_queue_with_manifest_write_mode,
};
pub use crate::layout::{
    StoryCoords, StoryLocationPartial, is_subagent_session_storage_key,
    is_trajectory_markdown_path, list_story_read_locations, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, merge_story_location,
    resolve_story_read_location, sanitize_session_filename, session_filename_stem,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    story_lance_event_path, story_run_dir, try_infer_story_location,
};

#[cfg(feature = "lance-store")]
pub use crate::discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    AppendOutcome, AttemptRecord, AttemptRecordState, AttemptRegistry, CatalogDataset,
    CatalogErrorPolicy, CatalogEventProvenance, CatalogEventView, CatalogNamespace, CatalogPage,
    CatalogProjectionStatus, CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind,
    CatalogSourceRevision, CatalogSourceStatus, CatalogStorylineKey, CatalogTrajectoryBundle,
    CommitRunOutcome, DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES,
    DEFAULT_DATASET_NAME, DEFAULT_MAX_EVENT_FALLBACK_BYTES, DEFAULT_MAX_EVENT_FALLBACK_ROWS,
    DEFAULT_PHYSICAL_PAGE_LIMIT, DatasetCatalogSnapshot, DatasetLocation, DatasetLocationKind,
    DatasetMount, DiscoveredSource, EventFactSnapshot, EventLogLayoutStats, EventWriterFence,
    ExportOutcome, LanceMaintenanceOptions, LanceMaintenanceReport, LeaseAcquireOutcome,
    NamespacePath, ObjectStoreManifestWriteMode, PhysicalColumn, PhysicalDataFile,
    PhysicalFileLayout, PhysicalFragment, PhysicalLayout, PhysicalPage, PhysicalPagePreview,
    PhysicalPageQuery, PhysicalSource, PhysicalTable, ProjectionSourceSnapshot,
    RawEventLanceAppender, RawEventLanceStore, ReplayOutcome, RunControlStore,
    StorylineContentOptions, StorylineContentReadMode, StorylineDataSource,
    StorylineDataSourceOptions, StorylineLanceStore, StorylineMaintenanceReport,
    StorylineProjectionLineage, StorylineStreamImportReport, StorylineTablePaths, TrajectoryStats,
    attempt_registry_now_ms, distinct_session_ids_in_run, export_source_dirs, export_story_bundle,
    inspect_physical_file, inspect_physical_layout, inspect_physical_page, list_physical_sources,
    raw_event_lance_path,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES, maintain_raw_events,
};

#[cfg(feature = "lance-store")]
pub use crate::projection::{
    AutomaticProjectionInspection, AutomaticProjectionInventory, AutomaticProjectionInventoryError,
    AutomaticProjectionMaintenanceMode, AutomaticProjectionMaintenanceReport,
    AutomaticProjectionState, AutomaticProjectionTarget, ProjectionRebuildReason,
    StorylineProjectionBuildOutcome, StorylineProjectionBuildReport, StorylineProjectionStatus,
    StorylineProjectionSyncMode, StorylineProjectionSyncOutcome, StorylineProjectionSyncReport,
    StorylineProjectionVerification, automatic_projection_inventory, build_storyline_projection,
    inspect_automatic_storyline_projection, maintain_automatic_storyline_projection,
    probe_canonical_event_store, rebuild_storyline_projection,
    storyline_projection_destination_exists, storyline_projection_status,
    sync_storyline_projection, verify_storyline_projection,
};

#[cfg(feature = "lance-store")]
pub use crate::revision::{RevisionRow, read_revisions, revision_dataset_path, write_revisions};
