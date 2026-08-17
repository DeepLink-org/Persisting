//! Rebuildable projections over the canonical pChronicle event log.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{
    project_event_records, EventRecord, RawEventLanceStore, StoryCoords, StorylineDocument,
};

use super::fs::{agenticmd_block_count, write_agenticmd_storyline};
use super::layout::{locate_session_markdown_for_key, session_markdown_path_for_key};

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

pub fn materialize_markdown_path(run_dir: &Path, session_key: &str) -> PathBuf {
    locate_session_markdown_for_key(run_dir, session_key)
        .unwrap_or_else(|| session_markdown_path_for_key(run_dir, session_key))
}

pub fn event_records_to_storyline(
    records: &[EventRecord],
) -> Result<(StorylineDocument, MaterializeStats)> {
    let eligible = project_dialogue_records(records);
    let story = if eligible.is_empty() {
        let session_id = records
            .iter()
            .rev()
            .find_map(|record| record.session_id.as_deref())
            .unwrap_or("unknown");
        let agent_id = records
            .iter()
            .rev()
            .find_map(|record| record.agent_id.as_deref())
            .unwrap_or("unknown");
        StorylineDocument::new(session_id, agent_id)
    } else {
        project_event_records(&eligible)?
    };
    let stats = MaterializeStats {
        source_events: records.len(),
        markdown_blocks: story.turns.len(),
        skipped_events: records.len().saturating_sub(eligible.len()),
    };
    Ok((story, stats))
}

/// Best-effort event → dialogue projection for human inspection.
///
/// This deliberately drops transport-only and duplicate streaming events.
/// AgenticMD is not a persistence boundary, so callers must retain the source
/// events or Storyline document when lossless replay is required.
fn project_dialogue_records(records: &[EventRecord]) -> Vec<EventRecord> {
    let mut last_user_message_count = 0usize;
    let mut skipped_call_ids = HashSet::new();
    let mut eligible = Vec::new();
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
            eligible.push(record.clone());
        }
    }
    eligible
}

pub fn write_markdown_projection(path: &Path, records: &[EventRecord]) -> Result<MaterializeStats> {
    let (story, stats) = event_records_to_storyline(records)?;
    write_agenticmd_storyline(path, &story)
        .with_context(|| format!("write markdown projection {}", path.display()))?;
    Ok(stats)
}

async fn load_events(session: &StoryCoords) -> Result<Vec<EventRecord>> {
    RawEventLanceStore
        .read_events(session, 0, None)
        .await
        .context("read canonical events")
}

pub async fn materialize_lance_to_markdown(session: &StoryCoords) -> Result<MaterializeOutcome> {
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

pub async fn layer_stats(session: &StoryCoords) -> Result<LayerStats> {
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
