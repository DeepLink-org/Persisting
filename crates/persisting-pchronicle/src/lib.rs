//! pChronicle — Persisting's structured storage layer for Agent trajectories.
//!
//! pChronicle owns the trajectory formats, physical schemas, storage backends,
//! replay, conversion, search, judgment, and rebuildable views. Capture and
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

#[cfg(feature = "lance-store")]
pub mod api;
pub mod atif;
pub mod convert;
#[cfg(feature = "lance-store")]
pub mod discovery;
pub mod error;
pub mod format;
pub mod formats;
pub mod interop;
#[cfg(feature = "lance-store")]
pub mod judge_service;
#[cfg(feature = "lance-store")]
pub mod judgment;
#[cfg(feature = "lance-store")]
pub mod judgment_summary;
pub mod layout;
pub mod mapping;
mod messages;
#[cfg(feature = "lance-store")]
pub mod operations;
#[cfg(feature = "lance-store")]
pub mod projection;
#[cfg(feature = "lance-store")]
pub mod revision;
#[cfg(feature = "lance-store")]
pub mod search;
#[cfg(feature = "lance-store")]
pub mod service;
pub mod store;
pub mod storyline_schema;

#[cfg(feature = "lance-store")]
pub use api::Chronicle;
pub use atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
pub use convert::{
    actf_to_storyline, actf_to_storylines, convert, events_to_storyline, from_storyline,
    into_storyline, is_actf_storyline, storyline_to_actf, storylines_to_actf,
};
#[cfg(feature = "lance-store")]
pub use discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};
pub use error::{classify_error, Error, ErrorCode, Result};
pub use format::ChronicleFormat;
pub use formats::events::EVENT_SCHEMA_VERSION;
pub use formats::{
    agenticmd_body_byte_offset, append_subagent_refs_footer, block_speaker, detect_format,
    encode_agenticmd_block, encode_agenticmd_document, encode_agenticmd_preamble,
    encode_agenticmd_session_frontmatter, events_lance_only_message, export_events_json_pretty,
    export_events_jsonl, is_subagent_footer_line, parse_agenticmd_blocks_with_spans,
    parse_agenticmd_document, parse_openai_msg_document, parse_storyline_document,
    strip_subagent_footer_from_body, validate_agenticmd_block, validate_speaker,
    validate_type_name, AgenticmdBlock, AgenticmdBlockSpan, AgenticmdClientMeta, AgenticmdDocument,
    AgenticmdHeader, AgenticmdSessionFrontmatter, EventIdentity, EventRecord, EventsDocument,
    OpenaiMsgCorpusReader, OpenaiMsgDocument, OpenaiMsgStep, RecoveredOpenaiMsgFile, StoryLink,
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FORMAT_NAME, AGENTICMD_FRONTMATTER_FORMAT, BLOCK_FORMAT_BLOCK, BLOCK_FORMAT_VERSION,
    BLOCK_MARKER, OPENAI_MSG_FORMAT_VERSION, STORYLINE_SCHEMA_VERSION,
};
pub use formats::{
    is_lossless_openai_storyline, parse_openai_msg_corpus_value, recover_openai_msg_files,
};
pub use formats::{
    parse_actf_document, ActfAssistantContent, ActfAttempt, ActfDocument, ActfMetric,
    ActfObservation, ActfStep, ActfToolCall, ActfTrajectory, ACTF_SCHEMA_VERSION,
};
pub use interop::{events_to_har, events_to_otlp_json, otlp_json_to_events};
#[cfg(feature = "lance-store")]
pub use judge_service::{
    judge_trajectory, JudgeTrajectoryOutcome, JudgeTrajectoryRequest, JudgingMethod,
};
#[cfg(feature = "lance-store")]
pub use judgment::{
    build_llm_judge_prompt, dataset_path as judgment_dataset_path, dialogue_judge_units,
    dry_run_judge_rows, evaluation_units, has_judgment, manual_few_shot_examples,
    manual_judge_rows, parse_llm_judge_rows, pending_evaluation_units, read_judge_rows,
    story_judge_body, write_judge_rows, EvaluationUnit, JudgeDialogueUnit, JudgeRow, JudgmentScope,
    ManualJudgmentInput, MANUAL_RATIONALE_PREFIX, STORY_CALL_ID,
};
#[cfg(feature = "lance-store")]
pub use judgment_summary::{
    aggregate_judgments, session_judgment_summary, JudgmentAggregate, JudgmentRubricSummary,
    JudgmentSessionSummary,
};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, list_story_read_locations,
    list_traj_read_locations, locate_run_bucket_markdown, locate_session_markdown,
    locate_session_markdown_for_key, merge_story_location, merge_traj_location,
    resolve_story_read_location, resolve_traj_read_location, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    story_lance_event_path, story_lance_judgment_path, story_run_dir, try_infer_story_location,
    try_infer_traj_location, StoryCoords, StoryLocationPartial, TrajLocation, TrajLocationPartial,
};
pub use mapping::{
    agenticmd_block_to_event_record, agenticmd_block_to_replay_json,
    agenticmd_blocks_to_event_records, enrich_event_from_agenticmd_block,
    event_record_to_agenticmd_block, event_record_to_agenticmd_block_with_text,
    markdown_document_to_event_records,
};
pub use messages::*;
#[cfg(feature = "lance-store")]
pub use operations::bridge::{
    search_add, search_add_batch, search_import_lance, search_index, search_index_delete,
    search_index_list, search_index_rebuild, search_index_reorder, search_query, trajectory_append,
    trajectory_replay, trajectory_stats as trajectory_stats_request,
};
#[cfg(feature = "lance-store")]
pub use operations::dispatch::invoke_request_body;
#[cfg(feature = "lance-store")]
pub use projection::{
    event_records_to_markdown_blocks, layer_stats, markdown_document_to_event_lines,
    materialize_lance_to_markdown, materialize_markdown_path, write_markdown_projection,
    LayerStats, MaterializeOutcome, MaterializeStats,
};
#[cfg(feature = "lance-store")]
pub use revision::{read_revisions, revision_dataset_path, write_revisions, RevisionRow};
#[cfg(feature = "lance-store")]
pub use search::agent as agent_search;
#[cfg(feature = "lance-store")]
pub use service::{
    append_trajectory, replay_trajectory, trajectory_stats, AppendServiceOutcome,
    ReplayServiceOutcome, StatsServiceOutcome,
};
#[cfg(feature = "lance-store")]
pub use store::maintain_raw_events;
pub use store::{
    agenticmd_block_count, agenticmd_replay_json_lines, agenticmd_structural_issues,
    append_agenticmd_blocks, count_agenticmd_role, encode_agenticmd_block_validated,
    find_block_by_call_id_and_role, index_agenticmd_path, list_agenticmd_paths,
    parse_agenticmd_document_validated, parse_agenticmd_spans_validated,
    read_agenticmd_blocks_from_file, rewrite_agenticmd_preamble, rewrite_block_range,
    upsert_block_by_call_id, write_agenticmd_document, AgenticmdFileIndex,
};
#[cfg(feature = "lance-store")]
pub use store::{
    attempt_registry_now_ms, decode_event_lines, distinct_session_ids_in_run, encode_event_lines,
    event_record_to_event_row, event_row_from_batch, event_row_to_event_record,
    event_row_to_replay_json, event_rows_from_batch, event_rows_to_batch, export_source_dirs,
    export_story_bundle, load_atif_trajectories, raw_event_arrow_schema, raw_event_lance_path,
    validate_event_lines, AppendOutcome, AtifDataSource, AtifDataSourceOptions, AtifReader,
    AttemptRecord, AttemptRecordState, AttemptRegistry, ChronicleQueryBackend,
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, CommitRunOutcome, EventLogLayoutStats,
    EventRow, EventWriterFence, ExportOutcome, ExternalTableFormat, ExternalTableSpec,
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryFormat,
    FileTrajectoryQueryMetrics, FileTrajectoryQueryMetricsSnapshot, LanceMaintenanceOptions,
    LanceMaintenanceReport, LeaseAcquireOutcome, LocalQueryInputFile, LocalQueryManifest,
    LocalQueryManifestOptions, RawEventDataSource, RawEventDataSourceOptions,
    RawEventLanceAppender, RawEventLanceStore, RawEventTableProvider, ReplayOutcome,
    RunControlStore, StorylineContentOptions, StorylineContentReadMode,
    StorylineDataFusionTableNames, StorylineDataSource, StorylineDataSourceOptions,
    StorylineLanceStore, StorylineMaintenanceReport, StorylineStreamImportReport,
    StorylineTableKind, StorylineTablePaths, StorylineTableProvider, StructuredStore,
    TrajectorySession, TrajectoryStats, DATAFUSION_EVENTS_TABLE, DATAFUSION_RUNS_TABLE,
    DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE, DEFAULT_CONTENT_OFFLOAD_THRESHOLD,
    DEFAULT_CONTENT_PREVIEW_BYTES, DEFAULT_LOCAL_QUERY_BATCH_SIZE, DEFAULT_LOCAL_QUERY_CACHE_BYTES,
    DEFAULT_LOCAL_QUERY_CACHE_FILES, DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES,
    DEFAULT_LOCAL_QUERY_MAX_RECORD_BYTES, DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES,
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES, SOURCE_FILE_COLUMN,
    TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_COLS, TRAJECTORY_EVENT_ID_COL,
    TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL, TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL, TRAJECTORY_TRACE_ID_COL,
};
#[cfg(feature = "lance-store")]
pub use store::{detect_local_query_format, detect_local_query_manifest};
pub use storyline_schema::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables, STORY_RUNS_TABLE, STORY_STEPS_TABLE, STORY_TOOL_CALLS_TABLE,
};

#[cfg(feature = "lance-store")]
pub const PERSISTING_VECTOR_INDEX_NAME: &str = search::search_lance::PERSISTING_VECTOR_INDEX_NAME;
#[cfg(feature = "lance-store")]
pub const PERSISTING_FTS_INDEX_NAME: &str = search::search_lance::PERSISTING_FTS_INDEX_NAME;

#[cfg(all(test, feature = "lance-store"))]
mod tests;
