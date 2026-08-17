//! AgenticMD debug-view domain: codec, mapping, paths, filesystem I/O, and projections.

mod codec;
mod convert;
mod fs;
mod layout;
#[cfg(feature = "lance-store")]
mod projection;
mod validate;

pub use convert::{encode_agenticmd, parse_agenticmd};
pub use fs::{
    agenticmd_block_count, agenticmd_structural_issues, count_agenticmd_role, index_agenticmd_path,
    list_agenticmd_paths, rewrite_agenticmd_storyline_metadata, upsert_agenticmd_turn,
    write_agenticmd_storyline, AgenticmdFileIndex,
};
pub use layout::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
};
#[cfg(feature = "lance-store")]
pub use projection::{
    layer_stats, materialize_lance_to_markdown, materialize_markdown_path,
    write_markdown_projection, LayerStats, MaterializeOutcome, MaterializeStats,
};
