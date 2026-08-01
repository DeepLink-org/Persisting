use anyhow::Result;
use persisting_pchronicle::{
    event_record_to_agenticmd_block_with_text, AgenticmdBlock, EventRecord,
};

use super::fields::role_and_body;
use crate::record::CaptureRecord;

/// Build a pChronicle agenticmd block from a capture record (primary write mapping).
///
/// Uses capture SSE-aware `visible_*` text, then pChronicle mapping (preserves call_id/seq).
pub fn capture_record_to_agenticmd_block(rec: &CaptureRecord) -> Result<AgenticmdBlock> {
    let (role, body) = role_and_body(rec)?;
    let event: EventRecord = rec.clone();
    event_record_to_agenticmd_block_with_text(&event, &role, &body)
}
