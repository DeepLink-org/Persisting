//! pChronicle — Persisting's structured storage layer for Agent trajectories.
//!
//! pChronicle owns the trajectory formats, physical schemas, storage backends,
//! replay, conversion, search, and rebuildable views. Capture and
//! clients call pChronicle directly; there is no separate storage engine layer.
//!
//! # Format architecture
//!
//! [`ChronicleFormat::Storyline`] is the **hub** (ATIF-aligned interchange).
//! All other formats convert only through storyline:
//!
//! ```text
//! events ──┐
//! agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif / actf
//! openai_msg┤
//! atif ─────┤
//! actf ─────┘
//! ```
//!
//! Use [`convert::into_storyline`] / [`convert::from_storyline`] / [`convert::convert`].

mod agenticmd;
#[cfg(feature = "lance-store")]
mod append_queue;
pub mod atif;
pub mod convert;
#[cfg(feature = "lance-store")]
pub mod discovery;
#[cfg(feature = "lance-store")]
pub mod document;
pub mod error;
pub mod format;
pub mod formats;
pub mod interop;
pub mod layout;
mod messages;
#[cfg(feature = "lance-store")]
pub mod operations;
#[cfg(feature = "lance-store")]
pub mod projection;
#[cfg(feature = "lance-store")]
pub mod revision;
#[cfg(feature = "search")]
pub mod search;
pub mod store;

pub use agenticmd::{
    agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role, encode_agenticmd,
    index_agenticmd_path, list_agenticmd_paths, parse_agenticmd,
    rewrite_agenticmd_storyline_metadata, upsert_agenticmd_turn, write_agenticmd_storyline,
    AgenticmdFileIndex,
};
#[cfg(feature = "lance-store")]
pub use append_queue::{
    raw_event_append_queue, raw_event_append_queue_with_capacity, RawEventAppendQueueError,
    RawEventAppendSender, RawEventAppendWorker, DEFAULT_RAW_EVENT_BATCH_DELAY,
    DEFAULT_RAW_EVENT_BATCH_SIZE, DEFAULT_RAW_EVENT_COMPACTION_THRESHOLD,
    DEFAULT_RAW_EVENT_HIERARCHY_FANOUT, DEFAULT_RAW_EVENT_MAINTENANCE_CAPACITY,
    DEFAULT_RAW_EVENT_QUEUE_CAPACITY, DEFAULT_RAW_EVENT_TARGET_ROWS_PER_FRAGMENT,
};
pub use atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
pub use convert::{
    actf_to_storyline, actf_to_storylines, convert, events_to_storyline, from_storyline,
    into_storyline, is_actf_storyline, project_event_records, storyline_to_actf,
    storylines_to_actf,
};
#[cfg(feature = "lance-store")]
pub use discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};
#[cfg(feature = "lance-store")]
pub use document::{
    open_document, DocumentSource, FilterPushdown, QueryCapabilities, QueryTables,
    DEFAULT_DOCUMENT_MATERIALIZE_BYTES, DEFAULT_DOCUMENT_MATERIALIZE_ROWS,
};
pub use error::{classify_error, Error, ErrorCode, Result};
pub use format::{ChronicleFormat, DocumentFormat};
pub use formats::{
    detect_format, events_lance_only_message, export_events_json_pretty, export_events_jsonl,
    parse_openai_msg_document, parse_storyline_document, ChronicleEventRecordExt, EventIdentity,
    EventRecord, EventsDocument, FieldPresence, LlmCandidate, LlmContentPart, LlmExtensions,
    LlmGenerationParams, LlmImageSource, LlmMessage, LlmProtocol, LlmRequest,
    LlmRequestEventPayload, LlmResponse, LlmResponseEventPayload, LlmResponseFormat, LlmRole,
    LlmStreamEvent, LlmToolChoice, LlmToolChoiceMode, LlmToolDefinition, LlmUsage,
    OpenaiMsgCorpusReader, OpenaiMsgDocument, OpenaiMsgStep, RecoveredOpenaiMsgFile, StoryLink,
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
pub use formats::{
    is_lossless_openai_storyline, parse_openai_msg_corpus_value, recover_openai_msg_files,
};
pub use formats::{
    parse_actf_document, ActfAssistantContent, ActfAttempt, ActfDocument, ActfMetric,
    ActfObservation, ActfStep, ActfToolCall, ActfTrajectory, ACTF_SCHEMA_VERSION,
};
pub use interop::{events_to_har, events_to_otlp_json, otlp_json_to_events};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, list_story_read_locations,
    locate_run_bucket_markdown, locate_session_markdown, locate_session_markdown_for_key,
    merge_story_location, resolve_story_read_location, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    story_lance_event_path, story_run_dir, try_infer_story_location, StoryCoords,
    StoryLocationPartial,
};
pub use messages::*;
#[cfg(feature = "search")]
pub use operations::bridge::{
    search_add, search_add_batch, search_import_lance, search_index, search_index_delete,
    search_index_list, search_index_rebuild, search_index_reorder, search_query,
};
#[cfg(feature = "search")]
pub use operations::dispatch::invoke_request_body;
#[cfg(feature = "lance-store")]
pub use projection::{
    build_storyline_projection, canonical_projection_lineage, event_records_to_storyline,
    layer_stats, materialize_lance_to_markdown, materialize_markdown_path,
    projection_lineage_is_fresh, rebuild_storyline_projection, storyline_projection_status,
    sync_storyline_projection, verify_storyline_projection, write_markdown_projection, LayerStats,
    MaterializeOutcome, MaterializeStats, StorylineProjectionBuildReport,
    StorylineProjectionStatus, StorylineProjectionSyncMode, StorylineProjectionSyncReport,
    StorylineProjectionVerification, STORYLINE_PROJECTION_COMPLETENESS, STORYLINE_PROJECTOR_NAME,
};
#[cfg(feature = "lance-store")]
pub use revision::{read_revisions, revision_dataset_path, write_revisions, RevisionRow};
#[cfg(feature = "search")]
pub use search::agent as agent_search;
#[cfg(feature = "lance-store")]
pub use store::maintain_raw_events;
#[cfg(feature = "lance-store")]
pub use store::{
    attempt_registry_now_ms, distinct_session_ids_in_run, event_record_to_event_row,
    event_records_from_batch, event_row_from_batch, event_row_to_event_record,
    event_rows_from_batch, event_rows_to_batch, export_source_dirs, export_story_bundle,
    load_atif_trajectories, raw_event_arrow_schema, raw_event_lance_path, AppendOutcome,
    AtifDataSource, AtifDataSourceOptions, AtifReader, AttemptRecord, AttemptRecordState,
    AttemptRegistry, CatalogDataset, CatalogErrorPolicy, CatalogNamespace, CatalogPage,
    CatalogProjectionStatus, CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind,
    CatalogSourceRevision, CatalogSourceStatus, CatalogStorylineKey, CatalogTrajectoryBundle,
    ChronicleQueryBackend, ChronicleQueryEngine, ChronicleQueryExecutionOptions, CommitRunOutcome,
    DatasetCatalogSnapshot, DatasetMount, DiscoveredSource, EventFactSnapshot, EventLogLayoutStats,
    EventRow, EventWriterFence, ExportOutcome, ExternalTableFormat, ExternalTableSpec,
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryFormat,
    FileTrajectoryQueryMetrics, FileTrajectoryQueryMetricsSnapshot, LanceMaintenanceOptions,
    LanceMaintenanceReport, LeaseAcquireOutcome, LocalQueryInputFile, LocalQueryManifest,
    LocalQueryManifestOptions, NamespacePath, ProjectionSourceSnapshot, RawEventDataSource,
    RawEventDataSourceOptions, RawEventLanceAppender, RawEventLanceStore, RawEventTableProvider,
    ReplayOutcome, RunControlStore, StorylineContentOptions, StorylineContentReadMode,
    StorylineDataFusionTableNames, StorylineDataSource, StorylineDataSourceOptions,
    StorylineLanceStore, StorylineMaintenanceReport, StorylineProjectionLineage,
    StorylineStreamImportReport, StorylineTableKind, StorylineTablePaths, StorylineTableProvider,
    TrajectoryStats, CATALOG_SOURCES_TABLE, CATALOG_TRAJECTORIES_TABLE, DATAFUSION_EVENTS_TABLE,
    DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE,
    DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_DATASET_NAME,
    DEFAULT_LOCAL_QUERY_BATCH_SIZE, DEFAULT_LOCAL_QUERY_CACHE_BYTES,
    DEFAULT_LOCAL_QUERY_CACHE_FILES, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
    DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES, DEFAULT_MAX_EVENT_FALLBACK_BYTES,
    DEFAULT_MAX_EVENT_FALLBACK_ROWS, DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES,
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES, SOURCE_FILE_COLUMN,
    TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_COLS, TRAJECTORY_EVENT_ID_COL,
    TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL, TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL, TRAJECTORY_TRACE_ID_COL,
};
#[cfg(feature = "lance-store")]
pub use store::{detect_local_query_format, detect_local_query_manifest};
pub use store::{reconstruct_storyline, split_storyline};
pub use store::{
    StoryRunRow, StoryStepRow, StoryToolCallRow, StorylineTables, STORY_RUNS_TABLE,
    STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};

#[cfg(feature = "search")]
pub const PERSISTING_VECTOR_INDEX_NAME: &str = search::search_lance::PERSISTING_VECTOR_INDEX_NAME;
#[cfg(feature = "search")]
pub const PERSISTING_FTS_INDEX_NAME: &str = search::search_lance::PERSISTING_FTS_INDEX_NAME;

#[cfg(all(test, feature = "lance-store"))]
mod tests;
