//! `CaptureRecord` ↔ markdown trajectory blocks.
//!
//! Markdown role/body text comes from Capture's [`crate::record::CaptureRecordExt`]
//! behavior (shared with turn indexing).

mod block;
mod draft;
mod fields;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use super::markdown_pipeline::skip_markdown_block;
pub use block::capture_record_to_agenticmd_block;
pub use draft::draft_stream_assistant_block;
