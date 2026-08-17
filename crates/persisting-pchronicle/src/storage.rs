//! pChronicle 的持久化存储入口。

pub use crate::error::{classify_error, Error, ErrorCode, Result};

pub use crate::layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, list_story_read_locations,
    locate_run_bucket_markdown, locate_session_markdown, locate_session_markdown_for_key,
    merge_story_location, resolve_story_read_location, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    story_lance_event_path, story_run_dir, try_infer_story_location, StoryCoords,
    StoryLocationPartial,
};
pub use crate::store::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};

#[cfg(feature = "lance-store")]
pub use crate::append_queue::{
    raw_event_append_queue, raw_event_append_queue_with_capacity, RawEventAppendQueueError,
    RawEventAppendSender, RawEventAppendWorker, DEFAULT_RAW_EVENT_BATCH_DELAY,
    DEFAULT_RAW_EVENT_BATCH_SIZE, DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
    DEFAULT_RAW_EVENT_HIERARCHY_FANOUT, DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY,
    DEFAULT_RAW_EVENT_QUEUE_CAPACITY, DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
};

#[cfg(feature = "lance-store")]
pub use crate::discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    attempt_registry_now_ms, distinct_session_ids_in_run, event_record_to_event_row,
    event_records_from_batch, event_row_from_batch, event_row_to_event_record,
    event_rows_from_batch, event_rows_to_batch, export_source_dirs, export_story_bundle,
    load_atif_trajectories, raw_event_arrow_schema, raw_event_lance_path, AppendOutcome,
    AtifReader, AttemptRecord, AttemptRecordState, AttemptRegistry, CatalogDataset,
    CatalogErrorPolicy, CatalogNamespace, CatalogPage, CatalogProjectionStatus,
    CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind, CatalogSourceRevision,
    CatalogSourceStatus, CatalogStorylineKey, CatalogTrajectoryBundle, CommitRunOutcome,
    DatasetCatalogSnapshot, DatasetMount, DiscoveredSource, EventFactSnapshot, EventLogLayoutStats,
    EventRow, EventWriterFence, ExportOutcome, LanceMaintenanceOptions, LanceMaintenanceReport,
    LeaseAcquireOutcome, NamespacePath, ProjectionSourceSnapshot, RawEventLanceAppender,
    RawEventLanceStore, ReplayOutcome, RunControlStore, StorylineContentOptions,
    StorylineContentReadMode, StorylineLanceStore, StorylineMaintenanceReport,
    StorylineProjectionLineage, StorylineStreamImportReport, StorylineTablePaths, TrajectoryStats,
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_DATASET_NAME,
    DEFAULT_MAX_EVENT_FALLBACK_BYTES, DEFAULT_MAX_EVENT_FALLBACK_ROWS,
};

#[cfg(feature = "lance-store")]
pub use crate::store::{
    detect_local_query_format, detect_local_query_manifest, maintain_raw_events,
    LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions,
    DEFAULT_LOCAL_QUERY_BATCH_SIZE, DEFAULT_LOCAL_QUERY_CACHE_BYTES,
    DEFAULT_LOCAL_QUERY_CACHE_FILES, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
    DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES, DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES,
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES,
};

#[cfg(feature = "lance-store")]
pub use crate::projection::{
    build_storyline_projection, canonical_projection_lineage, layer_stats,
    materialize_lance_to_markdown, materialize_markdown_path, projection_lineage_is_fresh,
    rebuild_storyline_projection, storyline_projection_status, sync_storyline_projection,
    verify_storyline_projection, write_markdown_projection, LayerStats, MaterializeOutcome,
    MaterializeStats, StorylineProjectionBuildReport, StorylineProjectionStatus,
    StorylineProjectionSyncMode, StorylineProjectionSyncReport, StorylineProjectionVerification,
    STORYLINE_PROJECTION_COMPLETENESS, STORYLINE_PROJECTOR_NAME,
};

#[cfg(feature = "lance-store")]
pub use crate::revision::{read_revisions, revision_dataset_path, write_revisions, RevisionRow};
