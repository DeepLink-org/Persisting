//! Session markdown (`{session_id}.md`): `<!-- persisting:block:{speaker} {json} -->` + body.

mod body;
mod io;
mod parse;
mod paths;
mod preamble;
mod types;

#[cfg(test)]
#[path = "tests_parser.rs"]
mod tests;
#[cfg(test)]
#[path = "tests_upsert.rs"]
mod upsert_tests;

pub use body::{strip_subagent_footer_from_body, BLOCK_FORMAT_BLOCK, BLOCK_FORMAT_VERSION};
pub use io::{
    append_engine_lines_to_markdown, encode_block_with_header, replay_json_lines,
    upsert_block_by_call_id,
};
pub use parse::{block_count, parse_document, read_blocks_from_file};
pub use paths::*;
pub use preamble::{document_body_start, format_document_preamble, format_duration_human};
pub(crate) use types::BLOCK_LAYOUT;
pub use types::{
    BlockHeader, MarkdownBlock, BLOCK_MARKER, LEGACY_TRAJECTORY_MARKDOWN_FILENAME,
    SESSION_MARKDOWN_FILENAME,
};
