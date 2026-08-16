use anyhow::Result;
use persisting_pchronicle::AgenticmdBlock;
use serde_json::json;

use super::block::capture_record_to_agenticmd_block;
use super::skip_markdown_block;
use crate::record::EventRecord;

/// Build a streaming draft assistant block (markdown view only; not written to Lance).
pub fn draft_stream_assistant_block(
    rec: &EventRecord,
    assistant_content: &str,
) -> Result<Option<AgenticmdBlock>> {
    if assistant_content.trim().is_empty() {
        return Ok(None);
    }
    let mut draft = rec.clone();
    draft.kind = "llm.response.stream".into();
    draft.payload = json!({
        "status": rec.payload.get("status").and_then(|v| v.as_u64()).unwrap_or(200),
        "assistant_content": assistant_content,
        "draft": true,
    });
    if skip_markdown_block(&draft) {
        return Ok(None);
    }
    Ok(Some(capture_record_to_agenticmd_block(&draft)?))
}
