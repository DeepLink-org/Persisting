//! `CaptureRecord` ↔ markdown trajectory blocks.
//!
//! Markdown role/body text comes from [`CaptureRecord::visible_user_text`] /
//! [`CaptureRecord::visible_assistant_text`] (shared with turn indexing).

mod block;
mod draft;
mod fields;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use crate::markdown_pipeline::skip_markdown_block;
pub use block::{
    agenticmd_block_to_capture_record, capture_record_to_agenticmd_block,
    markdown_document_to_capture_records_via_dialogue,
};
pub use draft::draft_stream_assistant_block;
