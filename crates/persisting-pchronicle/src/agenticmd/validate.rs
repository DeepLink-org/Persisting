//! Minimal safety validation for generated AgenticMD comments.
//!
//! Field presence and semantic combinations are deliberately not validated:
//! AgenticMD is a debugging view, not a protocol boundary.

use crate::{InputIssue, InputResult};

use super::codec::{MarkdownBlock, MarkdownHeader};

pub fn block_speaker(header: &MarkdownHeader) -> &str {
    header
        .fields
        .get("source")
        .and_then(|v| v.as_str())
        .or_else(|| header.fields.get("role").and_then(|v| v.as_str()))
        .unwrap_or("system")
}

pub fn validate_speaker(speaker: &str) -> InputResult<()> {
    let s = speaker.trim();
    if s.is_empty() {
        return Err(InputIssue::invalid("block speaker must not be empty"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(InputIssue::invalid(format!(
            "invalid block speaker: {speaker}"
        )));
    }
    Ok(())
}

pub fn validate_type_name(type_name: &str) -> InputResult<()> {
    let t = type_name.trim();
    if t.is_empty() {
        return Err(InputIssue::invalid("block type must not be empty"));
    }
    if t.contains('\n') || t.contains(':') {
        return Err(InputIssue::invalid(
            "block type must not contain ':' or newline",
        ));
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    {
        return Err(InputIssue::invalid(format!(
            "invalid block type: {type_name}"
        )));
    }
    Ok(())
}

pub fn validate_agenticmd_block(block: &MarkdownBlock) -> InputResult<()> {
    validate_type_name(&block.header.type_name)?;
    validate_speaker(block_speaker(&block.header))?;
    Ok(())
}
