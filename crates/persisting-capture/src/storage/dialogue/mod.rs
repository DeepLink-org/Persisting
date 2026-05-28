//! `CaptureRecord` ↔ markdown trajectory blocks.
//!
//! Markdown role/body text comes from [`CaptureRecord::visible_user_text`] /
//! [`CaptureRecord::visible_assistant_text`] (shared with turn indexing).

mod block;
mod draft;
mod fields;
mod import;
mod view;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use super::markdown_pipeline::skip_markdown_block;
pub use block::{
    block_to_capture_record, block_to_replay_json, capture_record_to_block, engine_line_to_block,
};
pub use draft::draft_stream_assistant_block;
pub use import::import_markdown_to_engine_lines;
pub use view::{capture_record_view, CaptureRecordView};

/// Returns `None` when the record should not appear in session markdown (stateless batch: no replay dedup).
pub fn try_capture_record_to_block(
    rec: &crate::record::CaptureRecord,
) -> anyhow::Result<Option<(super::markdown::BlockHeader, Vec<u8>)>> {
    super::markdown_pipeline::MarkdownPipeline::default().try_block(rec)
}

/// Batch convert records to markdown blocks (replay dedup applied in seq order).
pub fn capture_records_to_blocks(
    records: &[crate::record::CaptureRecord],
) -> anyhow::Result<Vec<(super::markdown::BlockHeader, Vec<u8>)>> {
    super::markdown_pipeline::MarkdownPipeline::blocks_from_records(records)
}
