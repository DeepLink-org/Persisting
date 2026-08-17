//! AgenticMD debug-view domain: codec, mapping, paths, filesystem I/O, and projections.

mod body;
mod codec;
mod convert;
mod frontmatter;
mod fs;
mod layout;
mod mapping;
#[cfg(feature = "lance-store")]
mod projection;
mod validate;

pub use body::{
    append_subagent_refs_footer, is_subagent_footer_line, strip_subagent_footer_from_body,
};
pub use codec::{
    agenticmd_body_byte_offset, encode_agenticmd_block, encode_agenticmd_document,
    encode_agenticmd_preamble, parse_agenticmd_blocks_with_spans, parse_agenticmd_document,
    AgenticmdBlock, AgenticmdBlockSpan, AgenticmdDocument, AgenticmdHeader, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FORMAT_NAME, AGENTICMD_FRONTMATTER_FORMAT, BLOCK_MARKER,
};
pub use convert::{agenticmd_to_storyline, storyline_to_agenticmd};
pub use frontmatter::{
    encode_agenticmd_session_frontmatter, AgenticmdClientMeta, AgenticmdSessionFrontmatter,
};
pub use fs::{
    agenticmd_block_count, agenticmd_replay_json_lines, agenticmd_structural_issues,
    append_agenticmd_blocks, count_agenticmd_role, encode_agenticmd_block_validated,
    find_block_by_call_id_and_role, index_agenticmd_path, list_agenticmd_paths,
    parse_agenticmd_document_validated, parse_agenticmd_spans_validated,
    read_agenticmd_blocks_from_file, rewrite_agenticmd_preamble, rewrite_block_range,
    upsert_block_by_call_id, write_agenticmd_document, AgenticmdFileIndex,
};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
};
pub use mapping::{
    agenticmd_block_to_event_record, agenticmd_block_to_replay_json,
    agenticmd_blocks_to_event_records, enrich_event_from_agenticmd_block,
    event_record_to_agenticmd_block, event_record_to_agenticmd_block_with_text,
    markdown_document_to_event_records,
};
#[cfg(feature = "lance-store")]
pub use projection::{
    event_records_to_markdown_blocks, layer_stats, materialize_lance_to_markdown,
    materialize_markdown_path, write_markdown_projection, LayerStats, MaterializeOutcome,
    MaterializeStats,
};
pub use validate::{block_speaker, validate_agenticmd_block, validate_speaker, validate_type_name};
