//! AgenticMD debug-view domain: codec, mapping, paths, and filesystem I/O.

mod codec;
mod convert;
mod fs;
mod layout;
mod validate;

pub use convert::{encode_agenticmd, parse_agenticmd};
pub use fs::{
    AgenticmdFileIndex, agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role,
    index_agenticmd_path, list_agenticmd_paths, rewrite_agenticmd_storyline_metadata,
    upsert_agenticmd_turn, write_agenticmd_storyline,
};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, sanitize_session_filename,
    session_filename_stem, session_markdown_filename, session_markdown_path_for_key,
    session_markdown_write_path_for_key,
};
