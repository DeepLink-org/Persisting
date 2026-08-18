//! Minimal safety validation for generated AgenticMD comments.
//!
//! Field presence and semantic combinations are deliberately not validated:
//! AgenticMD is a debugging view, not a protocol boundary.

use anyhow::{bail, Result};

use super::codec::{MarkdownBlock, MarkdownHeader};

pub fn block_speaker(header: &MarkdownHeader) -> &str {
    header
        .fields
        .get("source")
        .and_then(|v| v.as_str())
        .or_else(|| header.fields.get("role").and_then(|v| v.as_str()))
        .unwrap_or("system")
}

pub fn validate_speaker(speaker: &str) -> Result<()> {
    let s = speaker.trim();
    if s.is_empty() {
        bail!("block speaker must not be empty");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        bail!("invalid block speaker: {speaker}");
    }
    Ok(())
}

pub fn validate_type_name(type_name: &str) -> Result<()> {
    let t = type_name.trim();
    if t.is_empty() {
        bail!("block type must not be empty");
    }
    if t.contains('\n') || t.contains(':') {
        bail!("block type must not contain ':' or newline");
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    {
        bail!("invalid block type: {type_name}");
    }
    Ok(())
}

pub fn validate_agenticmd_block(block: &MarkdownBlock) -> Result<()> {
    validate_type_name(&block.header.type_name)?;
    validate_speaker(block_speaker(&block.header))?;
    Ok(())
}
