use anyhow::Result;
use serde_json::json;

use super::block::capture_record_to_block;
use super::skip_markdown_block;
use crate::record::CaptureRecord;
use crate::storage::markdown::BlockHeader;

/// Build a streaming draft assistant block (markdown view only; not written to Lance).
pub fn draft_stream_assistant_block(
    rec: &CaptureRecord,
    assistant_content: &str,
) -> Result<Option<(BlockHeader, Vec<u8>)>> {
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
    Ok(Some(capture_record_to_block(&draft)?))
}
