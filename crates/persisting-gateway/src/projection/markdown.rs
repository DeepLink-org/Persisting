//! Session markdown (`{session_id}.md`): live append/upsert + capture preamble.
//!
//! Layout path helpers and strict parse live in `persisting_pchronicle`; this
//! module keeps pipeline-aware IO and client-meta preamble.

use std::path::Path;

use anyhow::{Context, Result};
use persisting_pchronicle::{
    append_agenticmd_blocks, encode_agenticmd_preamble,
    upsert_block_by_call_id as chronicle_upsert, AgenticmdBlock, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FRONTMATTER_FORMAT,
};
use serde::Serialize;

use crate::session::client::{resolve_client_meta_for_run_dir, SessionClientMeta};

// --- preamble ----------------------------------------------------------------

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
        format: AGENTICMD_FRONTMATTER_FORMAT,
        block: AGENTICMD_BLOCK_LAYOUT,
        client,
    };
    encode_agenticmd_preamble(&doc)
        .map_err(|e| anyhow::anyhow!("pchronicle agenticmd preamble: {e}"))
}

/// Human-readable duration for frontmatter (`42s`, `3m12s`, `1h5m`).
pub(crate) fn format_duration_human(secs: u64) -> String {
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

fn write_agenticmd_blocks(path: &Path, blocks: &[AgenticmdBlock]) -> Result<usize> {
    let preamble = if !path.exists()
        || std::fs::metadata(path)
            .map(|m| m.len() == 0)
            .unwrap_or(true)
    {
        let client = path.parent().and_then(|run_dir| {
            run_dir
                .parent()
                .and_then(|agent_dir| agent_dir.parent())
                .and_then(|storage| resolve_client_meta_for_run_dir(storage, run_dir))
        });
        Some(format_document_preamble(client.as_ref())?)
    } else {
        None
    };
    append_agenticmd_blocks(path, blocks, preamble.as_deref())
}

/// Replace the block whose header `call_id` and `role` match, or append when missing.
pub fn upsert_block_by_call_id(path: &Path, call_id: &str, block: AgenticmdBlock) -> Result<bool> {
    // New files: seed capture preamble (with optional client meta) before first upsert.
    if !path.exists() {
        write_agenticmd_blocks(path, std::slice::from_ref(&block))?;
        return Ok(false);
    }
    chronicle_upsert(path, call_id, block).with_context(|| format!("upsert {}", path.display()))
}
