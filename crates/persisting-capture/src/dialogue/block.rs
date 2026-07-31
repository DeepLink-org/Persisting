use anyhow::{Context, Result};
use persisting_pchronicle::{
    agenticmd_block_to_event_record, agenticmd_blocks_to_event_records,
    event_record_to_agenticmd_block_with_text, AgenticmdBlock, EventRecord,
};

use super::fields::role_and_body;
use crate::record::CaptureRecord;

/// Build a pChronicle agenticmd block from a capture record (primary write mapping).
///
/// Uses capture SSE-aware `visible_*` text, then pChronicle mapping (preserves call_id/seq).
pub fn capture_record_to_agenticmd_block(rec: &CaptureRecord) -> Result<AgenticmdBlock> {
    let (role, body) = role_and_body(rec)?;
    let event = EventRecord::from(rec.clone());
    event_record_to_agenticmd_block_with_text(&event, &role, &body)
}

/// Reconstruct a capture record from a pChronicle agenticmd block (primary read mapping).
pub fn agenticmd_block_to_capture_record(block: &AgenticmdBlock) -> Result<CaptureRecord> {
    Ok(CaptureRecord::from(agenticmd_block_to_event_record(block)?))
}

fn agenticmd_blocks_to_capture_records(blocks: &[AgenticmdBlock]) -> Result<Vec<CaptureRecord>> {
    Ok(agenticmd_blocks_to_event_records(blocks)?
        .into_iter()
        .map(CaptureRecord::from)
        .collect())
}

/// Parse TLV / agenticmd markdown into capture records via pChronicle mapping.
pub fn markdown_document_to_capture_records_via_dialogue(doc: &str) -> Result<Vec<CaptureRecord>> {
    // Capture live docs use strict parse + speaker validation.
    let blocks = persisting_pchronicle::parse_agenticmd_document_validated(doc)?;
    agenticmd_blocks_to_capture_records(&blocks).with_context(|| "agenticmd blocks to records")
}
