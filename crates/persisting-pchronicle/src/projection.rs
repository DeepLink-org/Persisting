//! Rebuildable projections over the canonical pChronicle event log.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{
    agenticmd_block_count, encode_agenticmd_preamble, locate_session_markdown_for_key,
    markdown_document_to_event_records, session_markdown_path_for_key, write_agenticmd_document,
    EventRecord, RawEventLanceStore, StructuredStore, TrajectorySession, AGENTICMD_BLOCK_LAYOUT,
    AGENTICMD_FRONTMATTER_FORMAT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeStats {
    pub source_events: usize,
    pub markdown_blocks: usize,
    pub skipped_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeOutcome {
    pub markdown_path: String,
    pub stats: MaterializeStats,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStats {
    pub event_rows: usize,
    pub markdown_blocks: usize,
    pub event_log_path: String,
    pub markdown_path: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncateOutcome {
    pub event_log_path: String,
    pub kept_rows: usize,
    pub removed_rows: usize,
    pub note: String,
}

#[derive(serde::Serialize)]
struct ProjectionPreamble {
    format: &'static str,
    block: &'static str,
}

pub fn materialize_markdown_path(run_dir: &Path, session_key: &str) -> PathBuf {
    locate_session_markdown_for_key(run_dir, session_key)
        .unwrap_or_else(|| session_markdown_path_for_key(run_dir, session_key))
}

pub fn event_records_to_markdown_blocks(
    records: &[EventRecord],
) -> Result<(Vec<crate::AgenticmdBlock>, MaterializeStats)> {
    let blocks = project_dialogue_blocks(records)?;
    let stats = MaterializeStats {
        source_events: records.len(),
        markdown_blocks: blocks.len(),
        skipped_events: records.len().saturating_sub(blocks.len()),
    };
    Ok((blocks, stats))
}

/// Best-effort event → dialogue projection for human inspection.
///
/// This deliberately drops transport-only and duplicate streaming events.
/// AgenticMD is not a persistence boundary, so callers must retain the source
/// events or Storyline document when lossless replay is required.
fn project_dialogue_blocks(records: &[EventRecord]) -> Result<Vec<crate::AgenticmdBlock>> {
    let mut last_user_message_count = 0usize;
    let mut skipped_call_ids = HashSet::new();
    let mut blocks = Vec::new();
    for record in records {
        let skip = match record.kind.as_str() {
            "llm.request" | "http.request" => {
                let protocol = record.payload.get("protocol").and_then(|v| v.as_str());
                let path = record.payload.get("path").and_then(|v| v.as_str());
                let internal = protocol == Some("count_tokens")
                    || path.is_some_and(|p| p.ends_with("/count_tokens"));
                if internal {
                    true
                } else if record
                    .payload
                    .get("user_message_count")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|count| {
                        let replay = count as usize <= last_user_message_count;
                        if !replay {
                            last_user_message_count = count as usize;
                        }
                        replay
                    })
                {
                    if let Some(call_id) = &record.call_id {
                        skipped_call_ids.insert(call_id.clone());
                    }
                    true
                } else {
                    false
                }
            }
            "llm.response" | "llm.response.stream" | "http.response" | "http.response.stream" => {
                record
                    .call_id
                    .as_ref()
                    .is_some_and(|id| skipped_call_ids.contains(id))
                    || record
                        .payload
                        .get("stream_partial")
                        .and_then(|v| v.as_bool())
                        == Some(true)
            }
            "llm.call.cancelled" | "http.cancel" => true,
            kind if kind.starts_with("session.") => true,
            _ => false,
        };
        if !skip {
            let block = crate::event_record_to_agenticmd_block(record)?;
            if !block.body.trim().is_empty() || record.kind == "llm.spawn_link" {
                blocks.push(block);
            }
        }
    }
    Ok(blocks)
}

pub fn write_markdown_projection(path: &Path, records: &[EventRecord]) -> Result<MaterializeStats> {
    let (blocks, stats) = event_records_to_markdown_blocks(records)?;
    let preamble = encode_agenticmd_preamble(&ProjectionPreamble {
        format: AGENTICMD_FRONTMATTER_FORMAT,
        block: AGENTICMD_BLOCK_LAYOUT,
    })?;
    write_agenticmd_document(path, &preamble, &blocks)
        .with_context(|| format!("write markdown projection {}", path.display()))?;
    Ok(stats)
}

pub fn markdown_document_to_event_lines(document: &str) -> Result<Vec<String>> {
    markdown_document_to_event_records(document)?
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            let value = serde_json::to_value(record)
                .with_context(|| format!("serialize markdown event[{index}]"))?;
            ron::to_string(&value).with_context(|| format!("encode markdown event[{index}] RON"))
        })
        .collect()
}

async fn load_events(session: &TrajectorySession) -> Result<Vec<EventRecord>> {
    RawEventLanceStore
        .read_events(session, 0, None)
        .await
        .context("read canonical events")
}

pub async fn materialize_lance_to_markdown(
    session: &TrajectorySession,
) -> Result<MaterializeOutcome> {
    if !RawEventLanceStore.exists(session).await? {
        anyhow::bail!(
            "canonical event log missing at {}",
            RawEventLanceStore.display_path(session)?
        );
    }
    let events = load_events(session).await?;
    let run_dir = session.run_dir()?;
    let path = materialize_markdown_path(&run_dir, &session.session_id);
    let stats = write_markdown_projection(&path, &events)?;
    Ok(MaterializeOutcome {
        markdown_path: path.display().to_string(),
        note: format!(
            "Materialized pChronicle events→AgenticMD: {} event(s) → {} block(s), skipped {} at {}",
            stats.source_events,
            stats.markdown_blocks,
            stats.skipped_events,
            path.display()
        ),
        stats,
    })
}

pub async fn layer_stats(session: &TrajectorySession) -> Result<LayerStats> {
    let event_rows = if RawEventLanceStore.exists(session).await? {
        RawEventLanceStore.stats(session).await?.row_count
    } else {
        0
    };
    let run_dir = session.run_dir()?;
    let markdown_path = locate_session_markdown_for_key(&run_dir, &session.session_id);
    let markdown_blocks = markdown_path
        .as_ref()
        .map(|path| agenticmd_block_count(path))
        .transpose()?
        .unwrap_or(0);
    let note = match (event_rows > 0, markdown_blocks > 0) {
        (true, true) => format!(
            "pChronicle canonical events {event_rows}; AgenticMD projection {markdown_blocks}"
        ),
        (true, false) => format!("pChronicle canonical events only: {event_rows}"),
        (false, true) => format!(
            "Canonical events missing; non-authoritative AgenticMD debug view: {markdown_blocks}"
        ),
        (false, false) => "No trajectory data yet".into(),
    };
    Ok(LayerStats {
        event_rows,
        markdown_blocks,
        event_log_path: RawEventLanceStore.display_path(session)?,
        markdown_path: markdown_path.map(|path| path.display().to_string()),
        note,
    })
}

/// Keep the first `keep_rows` events in one logical Lance session partition.
pub async fn truncate_lance_session(
    session: &TrajectorySession,
    keep_rows: usize,
) -> Result<TruncateOutcome> {
    let store = RawEventLanceStore;
    let event_log_path = store.display_path(session)?;
    let events = store.read_events(session, 0, None).await?;
    let total = events.len();
    let keep = keep_rows.min(total);
    let persisted = crate::overwrite_session_events(session, &events[..keep]).await?;
    Ok(TruncateOutcome {
        event_log_path: event_log_path.clone(),
        kept_rows: persisted,
        removed_rows: total.saturating_sub(persisted),
        note: format!("truncated Lance: kept {persisted}/{total} row(s) at {event_log_path}"),
    })
}
