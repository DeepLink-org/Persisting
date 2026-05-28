//! YAML document preamble (distinct from session rollup frontmatter).

use anyhow::{Context, Result};
use serde::Serialize;

use super::parse::skip_preamble;
use super::types::BLOCK_LAYOUT;
use crate::storage::session_client::SessionClientMeta;

#[derive(Serialize)]
struct DocumentFrontmatter<'a> {
    format: &'static str,
    block: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<&'a SessionClientMeta>,
}

/// Build YAML frontmatter; includes detected client process when available.
pub fn format_document_preamble(client: Option<&SessionClientMeta>) -> Result<String> {
    let doc = DocumentFrontmatter {
        format: "persisting:1.0",
        block: BLOCK_LAYOUT,
        client,
    };
    let yaml = serde_yaml::to_string(&doc).context("serialize trajectory frontmatter")?;
    Ok(format!("---\n{yaml}---\n\n"))
}

/// Human-readable duration for frontmatter (`42s`, `3m12s`, `1h5m`).
pub fn format_duration_human(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{mins}m")
    }
}

/// Byte offset in `content` where the first trajectory block begins (after YAML preamble).
pub fn document_body_start(content: &str) -> Result<usize> {
    skip_preamble(content.as_bytes(), 0)
}
