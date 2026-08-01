//! pChronicle — Persisting's structured storage layer for Agent trajectories.
//!
//! pChronicle owns the trajectory formats, physical schemas, storage backends,
//! replay, conversion, and rebuildable views. Capture produces [`EventRecord`]s;
//! Engine and CLI delegate persistence and format operations to this crate.
//!
//! # Format architecture
//!
//! [`ChronicleFormat::Storyline`] is the **hub** (ATIF-aligned interchange).
//! All other formats convert only through storyline:
//!
//! ```text
//! events ──┐
//! agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif
//! openai_msg┤
//! atif ─────┘
//! ```
//!
//! Use [`convert::into_storyline`] / [`convert::from_storyline`] / [`convert::convert`].

pub mod atif;
pub mod convert;
pub mod discovery;
pub mod error;
pub mod format;
pub mod formats;
pub mod ingest;
pub mod judge_service;
pub mod judgment;
pub mod judgment_summary;
pub mod layout;
pub mod mapping;
pub mod projection;
pub mod schema;
pub mod selection;
pub mod service;
pub mod store;
pub mod view;

pub use atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
pub use convert::{convert, from_storyline, into_storyline};
pub use discovery::{
    drop_lifecycle_run_partitions, expand_story_locations, expand_story_locations_blocking,
};
pub use error::{Error, Result};
pub use format::ChronicleFormat;
pub use formats::{
    agenticmd_body_byte_offset, append_subagent_refs_footer, block_speaker, detect_format,
    encode_agenticmd_block, encode_agenticmd_document, encode_agenticmd_preamble,
    encode_agenticmd_session_frontmatter, events_lance_only_message, export_events_json_pretty,
    export_events_jsonl, is_subagent_footer_line, parse_agenticmd_blocks_with_spans,
    parse_agenticmd_document, parse_agenticmd_document_with, parse_openai_msg_document,
    parse_storyline_document, strip_subagent_footer_from_body, validate_agenticmd_block,
    validate_speaker, validate_type_name, AgenticmdBlock, AgenticmdBlockSpan, AgenticmdClientMeta,
    AgenticmdDocument, AgenticmdHeader, AgenticmdParseMode, AgenticmdSessionFrontmatter,
    EventRecord, EventsDocument, OpenaiMsgDocument, OpenaiMsgStep, StoryLink, StorylineAgent,
    StorylineDocument, StorylineToolCall, StorylineTurn, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FORMAT_NAME, AGENTICMD_FRONTMATTER_FORMAT, BLOCK_FORMAT_BLOCK, BLOCK_FORMAT_VERSION,
    BLOCK_MARKER, OPENAI_MSG_FORMAT_VERSION, STORYLINE_SCHEMA_VERSION,
};
pub use ingest::{ingest_trajectory, reconstruct_trajectory, split_trajectory, SplitTables};
pub use judge_service::{
    judge_trajectory, JudgeTrajectoryOutcome, JudgeTrajectoryRequest, JudgingMethod,
};
pub use judgment::{
    build_llm_judge_prompt, dataset_path as judgment_dataset_path, dialogue_judge_units,
    dry_run_judge_rows, evaluation_units, has_judgment, layer_field_name, manual_few_shot_examples,
    manual_judge_rows, parse_llm_judge_rows, pending_evaluation_units, read_judge_rows,
    story_judge_body, write_judge_rows, EvaluationUnit, JudgeDialogueUnit, JudgeRow, JudgmentScope,
    ManualJudgmentInput, MANUAL_RATIONALE_PREFIX, STORY_CALL_ID,
};
pub use judgment_summary::{
    aggregate_judgments, session_judgment_summary, JudgmentAggregate, JudgmentRubricSummary,
    JudgmentSessionSummary,
};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, list_story_read_locations,
    list_traj_read_locations, locate_run_bucket_markdown, locate_session_markdown,
    locate_session_markdown_for_key, merge_story_location, merge_traj_location,
    resolve_story_read_location, resolve_traj_read_location, sanitize_session_filename,
    session_markdown_filename, session_markdown_path, session_markdown_path_for_key,
    session_markdown_write_path, session_markdown_write_path_for_key, story_lance_event_path,
    story_run_dir, try_infer_story_location, try_infer_traj_location, StoryCoords,
    StoryLocationPartial, TrajLocation, TrajLocationPartial, LEGACY_TRAJECTORY_MARKDOWN_FILENAME,
    SESSION_MARKDOWN_FILENAME,
};
pub use mapping::{
    agenticmd_block_to_event_record, agenticmd_block_to_replay_json,
    agenticmd_blocks_to_event_records, enrich_event_from_agenticmd_block,
    event_record_to_agenticmd_block, event_record_to_agenticmd_block_with_text,
    markdown_document_to_event_records,
};
pub use projection::{
    compact_markdown_to_lance, event_records_to_markdown_blocks, layer_stats,
    markdown_document_to_event_lines, materialize_lance_to_markdown, materialize_markdown_path,
    truncate_lance_session, write_markdown_projection, CompactOutcome, CompactStats, LayerStats,
    MaterializeOutcome, MaterializeStats, TruncateOutcome,
};
pub use schema::{SessionRow, StepRow, ToolCallRow};
pub use selection::{
    dataset_display, detect_primary_layer, resolve_for_append as resolve_storage_for_append,
    resolve_for_read as resolve_storage_for_read, selection_label, story_stats_note,
    StorageSelection,
};
pub use service::{
    append_trajectory, replay_trajectory, trajectory_stats, AppendServiceOutcome,
    ReplayServiceOutcome, StatsServiceOutcome,
};
pub use store::{
    agenticmd_block_count, agenticmd_replay_json_lines, agenticmd_structural_issues,
    append_agenticmd_blocks, count_agenticmd_role, decode_event_lines, distinct_session_ids_in_run,
    encode_agenticmd_block_validated, encode_event_lines, event_record_to_event_row,
    event_row_from_batch, event_row_to_event_record, event_row_to_replay_json,
    event_rows_from_batch, event_rows_to_batch, export_source_dirs, export_story_bundle,
    find_block_by_call_id_and_role, index_agenticmd_path, list_agenticmd_paths,
    overwrite_session_events, overwrite_session_lines, parse_agenticmd_document_validated,
    parse_agenticmd_spans_validated, parse_engine_records, read_agenticmd_blocks_from_file,
    rewrite_agenticmd_preamble, rewrite_block_range, session_lance_path, structured_store,
    trajectory_arrow_schema, upsert_block_by_call_id, write_agenticmd_document, AgenticMdStore,
    AgenticmdFileIndex, AppendOutcome, ChronicleStore, EventRow, ExportOutcome, FsChronicleStore,
    LanceEventStore, LanceTrajectoryStore, MarkdownTrajectoryStore, MemoryChronicleStore,
    NormalizedStore, ReplayOutcome, StorageKind, StructuredStore, TrajectoryAppendOutcome,
    TrajectoryReplayOutcome, TrajectorySession, TrajectoryStats, TrajectoryStatsOutcome,
    TrajectoryStore, TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_KIND_COL,
    TRAJECTORY_MODEL_COL, TRAJECTORY_PARENT_CALL_ID_COL, TRAJECTORY_PAYLOAD_JSON_COL,
    TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL, TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL,
    TRAJECTORY_TRACE_ID_COL, TRAJECTORY_V1_COLS,
};
pub use view::{atif_trajectory_sql_ddl, AtifTrajectoryView, AtifViewRow, ATIF_TRAJECTORY_VIEW};

#[cfg(test)]
mod tests;
