//! Rebuildable projections over the canonical pChronicle event log.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::store::markdown::project_dialogue_blocks;
use crate::{
    agenticmd_block_count, encode_agenticmd_preamble, locate_session_markdown_for_key,
    markdown_document_to_event_records, session_markdown_path_for_key, write_agenticmd_document,
    AgenticMdStore, EventRecord, LanceEventStore, StructuredStore, TrajectorySession,
    AGENTICMD_BLOCK_LAYOUT, AGENTICMD_FRONTMATTER_FORMAT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeStats {
    pub source_events: usize,
    pub markdown_blocks: usize,
    pub skipped_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub source_blocks: usize,
    pub event_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeOutcome {
    pub markdown_path: String,
    pub stats: MaterializeStats,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactOutcome {
    pub event_log_path: String,
    pub stats: CompactStats,
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
    LanceEventStore
        .read_events(session, 0, None)
        .await
        .context("read canonical events")
}

pub async fn materialize_lance_to_markdown(
    session: &TrajectorySession,
) -> Result<MaterializeOutcome> {
    if !LanceEventStore.exists(session).await? {
        anyhow::bail!(
            "canonical event log missing at {}",
            LanceEventStore.display_path(session)?
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

pub async fn compact_markdown_to_lance(
    session: &TrajectorySession,
    overwrite: bool,
) -> Result<CompactOutcome> {
    if !AgenticMdStore.exists(session).await? {
        anyhow::bail!(
            "AgenticMD projection missing at {}",
            AgenticMdStore.display_path(session)?
        );
    }
    let run_dir = session.run_dir()?;
    let path = locate_session_markdown_for_key(&run_dir, &session.session_id)
        .ok_or_else(|| anyhow::anyhow!("AgenticMD not found under {}", run_dir.display()))?;
    let document = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read {}", path.display()))?;
    let records = markdown_document_to_event_records(&document)?;
    let event_log_path = LanceEventStore.display_path(session)?;
    let event_rows = if overwrite {
        let lines = crate::encode_event_lines(&records)?;
        crate::overwrite_session_lines(session, &lines).await?
    } else {
        LanceEventStore
            .append_events(session, &records)
            .await?
            .persisted_units
    };
    let stats = CompactStats {
        source_blocks: records.len(),
        event_rows,
    };
    Ok(CompactOutcome {
        event_log_path: event_log_path.clone(),
        note: format!(
            "Compacted AgenticMD→pChronicle events: {} block(s) → {} row(s) ({}) at {} → {}",
            stats.source_blocks,
            stats.event_rows,
            if overwrite { "overwrite" } else { "append" },
            path.display(),
            event_log_path
        ),
        stats,
    })
}

pub async fn layer_stats(session: &TrajectorySession) -> Result<LayerStats> {
    let event_rows = if LanceEventStore.exists(session).await? {
        LanceEventStore.stats(session).await?.row_count
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
        (false, true) => format!("AgenticMD projection only: {markdown_blocks}"),
        (false, false) => "No trajectory data yet".into(),
    };
    Ok(LayerStats {
        event_rows,
        markdown_blocks,
        event_log_path: LanceEventStore.display_path(session)?,
        markdown_path: markdown_path.map(|path| path.display().to_string()),
        note,
    })
}

/// Keep the first `keep_rows` events in one logical Lance session partition.
pub async fn truncate_lance_session(
    session: &TrajectorySession,
    keep_rows: usize,
) -> Result<TruncateOutcome> {
    let store = LanceEventStore;
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
