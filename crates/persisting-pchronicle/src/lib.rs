//! pChronicle — Canonical Run History Store.
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
pub mod error;
pub mod format;
pub mod formats;
pub mod ingest;
pub mod layout;
pub mod mapping;
pub mod schema;
pub mod store;
pub mod view;

pub use atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
pub use convert::{convert, from_storyline, into_storyline};
pub use error::{Error, Result};
pub use format::ChronicleFormat;
pub use formats::{
    agenticmd_body_byte_offset, append_subagent_refs_footer, block_speaker, detect_format,
    encode_agenticmd_block, encode_agenticmd_document, encode_agenticmd_preamble,
    events_lance_only_message, export_events_json_pretty, export_events_jsonl,
    is_subagent_footer_line, parse_agenticmd_blocks_with_spans, parse_agenticmd_document,
    parse_agenticmd_document_with, parse_openai_msg_document, parse_storyline_document,
    strip_subagent_footer_from_body, validate_agenticmd_block, validate_speaker,
    validate_type_name, AgenticmdBlock, AgenticmdBlockSpan, AgenticmdDocument, AgenticmdHeader,
    AgenticmdParseMode, EventRecord, EventsDocument, OpenaiMsgDocument, OpenaiMsgStep, StoryLink,
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FORMAT_NAME, AGENTICMD_FRONTMATTER_FORMAT, BLOCK_FORMAT_BLOCK, BLOCK_FORMAT_VERSION,
    BLOCK_MARKER, OPENAI_MSG_FORMAT_VERSION, STORYLINE_SCHEMA_VERSION,
};
pub use ingest::{ingest_trajectory, reconstruct_trajectory, split_trajectory, SplitTables};
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
pub use schema::{SessionRow, StepRow, ToolCallRow};
pub use store::{
    agenticmd_block_count, append_agenticmd_blocks, encode_agenticmd_block_validated,
    event_record_to_event_row, event_row_to_event_record, event_row_to_replay_json,
    export_source_dirs, export_story_bundle, find_block_by_call_id_and_role,
    parse_agenticmd_document_validated, parse_agenticmd_spans_validated, parse_engine_records,
    read_agenticmd_blocks_from_file, rewrite_block_range, upsert_block_by_call_id, ChronicleStore,
    EventLogStore, EventRow, ExportOutcome, FsChronicleStore, MemoryChronicleStore,
};
pub use view::{atif_trajectory_sql_ddl, AtifTrajectoryView, AtifViewRow, ATIF_TRAJECTORY_VIEW};

#[cfg(test)]
mod tests;
