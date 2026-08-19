//! pChronicle 的持久化存储入口。

pub type Result<T> = anyhow::Result<T>;

#[cfg(feature = "lance-store")]
pub use crate::append_queue::{
    raw_event_append_queue, raw_event_append_queue_with_capacity, RawEventAppendOutcome,
    RawEventAppendSender, RawEventAppendWorker, DEFAULT_RAW_EVENT_BATCH_DELAY,
    DEFAULT_RAW_EVENT_BATCH_SIZE, DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
    DEFAULT_RAW_EVENT_HIERARCHY_FANOUT, DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY,
    DEFAULT_RAW_EVENT_QUEUE_CAPACITY, DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
};
pub use crate::layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, list_story_read_locations,
    locate_run_bucket_markdown, locate_session_markdown, locate_session_markdown_for_key,
    merge_story_location, resolve_story_read_location, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    story_lance_event_path, story_run_dir, try_infer_story_location, StoryCoords,
    StoryLocationPartial,
};

#[cfg(feature = "lance-store")]
pub use crate::discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    attempt_registry_now_ms, distinct_session_ids_in_run, export_source_dirs, export_story_bundle,
    raw_event_lance_path, AppendOutcome, AttemptRecord, AttemptRecordState, AttemptRegistry,
    CatalogDataset, CatalogErrorPolicy, CatalogNamespace, CatalogPage, CatalogProjectionStatus,
    CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind, CatalogSourceRevision,
    CatalogSourceStatus, CatalogStorylineKey, CatalogTrajectoryBundle, CommitRunOutcome,
    DatasetCatalogSnapshot, DatasetMount, DiscoveredSource, EventFactSnapshot, EventLogLayoutStats,
    EventWriterFence, ExportOutcome, LanceMaintenanceOptions, LanceMaintenanceReport,
    LeaseAcquireOutcome, NamespacePath, ProjectionSourceSnapshot, RawEventLanceAppender,
    RawEventLanceStore, ReplayOutcome, RunControlStore, StorylineContentOptions,
    StorylineContentReadMode, StorylineLanceStore, StorylineMaintenanceReport,
    StorylineProjectionLineage, StorylineStreamImportReport, StorylineTablePaths, TrajectoryStats,
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_DATASET_NAME,
    DEFAULT_MAX_EVENT_FALLBACK_BYTES, DEFAULT_MAX_EVENT_FALLBACK_ROWS,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    maintain_raw_events, DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES,
};

#[cfg(feature = "lance-store")]
pub use crate::projection::{
    build_storyline_projection, rebuild_storyline_projection, storyline_projection_status,
    sync_storyline_projection, verify_storyline_projection, ProjectionRebuildReason,
    StorylineProjectionBuildOutcome, StorylineProjectionBuildReport, StorylineProjectionStatus,
    StorylineProjectionSyncMode, StorylineProjectionSyncOutcome, StorylineProjectionSyncReport,
    StorylineProjectionVerification,
};

#[cfg(feature = "lance-store")]
pub use crate::revision::{read_revisions, revision_dataset_path, write_revisions, RevisionRow};
