//! Vortex ↔ TLV Markdown conversion at the trajectory storage layer.

use anyhow::{Context, Result};
use persisting_capture::trajectory_convert::{
    compact_stats_note, markdown_document_to_engine_lines, materialize_markdown_path,
    materialize_records_to_markdown, CompactStats, MaterializeStats,
};

use super::store::{
    overwrite_session_lines, MarkdownTrajectoryStore, TrajectorySession, TrajectoryStore,
    VortexTrajectoryStore,
};
use persisting_capture::story_coords::story_run_dir;

/// Result of materializing Vortex raw log → TLV Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeOutcome {
    pub markdown_path: String,
    pub stats: MaterializeStats,
    pub note: String,
}

/// Result of compacting TLV Markdown → Vortex raw log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactOutcome {
    pub event_log_path: String,
    pub stats: CompactStats,
    pub note: String,
}

async fn load_all_capture_records(
    session: &TrajectorySession,
) -> Result<Vec<persisting_capture::record::CaptureRecord>> {
    let vortex = VortexTrajectoryStore;
    let outcome = vortex
        .replay(session, 0, None)
        .await
        .context("replay Vortex for conversion")?;
    outcome
        .records
        .iter()
        .enumerate()
        .map(|(i, json)| {
            serde_json::from_str::<persisting_capture::record::CaptureRecord>(json)
                .with_context(|| format!("decode replay record[{i}]"))
        })
        .collect()
}

/// **Vortex → Markdown** (lossy): scan raw event log, write dialogue TLV blocks.
pub async fn materialize_vortex_to_markdown(
    session: &TrajectorySession,
) -> Result<MaterializeOutcome> {
    let vortex = VortexTrajectoryStore;
    if !vortex.exists(session).await? {
        anyhow::bail!(
            "Vortex event log missing at {}; materialize requires raw event log",
            vortex.display_path(session)?
        );
    }

    let records = load_all_capture_records(session).await?;
    let run_dir = story_run_dir(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )?;
    let md_path = materialize_markdown_path(&run_dir, &session.session_id);
    let stats = materialize_records_to_markdown(&md_path, &records)?;

    Ok(MaterializeOutcome {
        markdown_path: md_path.display().to_string(),
        note: format!(
            "Materialized Vortex→Markdown (lossy): {} event(s) → {} block(s), skipped {} internal/non-dialogue event(s) at {}",
            stats.source_events,
            stats.markdown_blocks,
            stats.skipped_events,
            md_path.display()
        ),
        stats,
    })
}

/// **Markdown → Vortex** (compact): parse TLV blocks into event log rows.
pub async fn compact_markdown_to_vortex(
    session: &TrajectorySession,
    overwrite: bool,
) -> Result<CompactOutcome> {
    let markdown = MarkdownTrajectoryStore;
    if !markdown.exists(session).await? {
        anyhow::bail!(
            "Markdown session file missing at {}; compact requires TLV markdown",
            markdown.display_path(session)?
        );
    }

    let run_dir = story_run_dir(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )?;
    let md_path = persisting_capture::markdown_trajectory::locate_session_markdown_for_key(
        &run_dir,
        &session.session_id,
    )
    .ok_or_else(|| anyhow::anyhow!("markdown not found under {}", run_dir.display()))?;
    let doc = tokio::fs::read_to_string(&md_path)
        .await
        .with_context(|| format!("read {}", md_path.display()))?;

    let engine_lines = markdown_document_to_engine_lines(&doc)?;
    let line_vec: Vec<String> = engine_lines
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let source_blocks = line_vec.len();

    let vortex = VortexTrajectoryStore;
    let event_log_path = vortex.display_path(session)?;

    let event_rows = if overwrite {
        overwrite_session_lines(session, &line_vec).await?
    } else {
        VortexTrajectoryStore
            .append(session, &line_vec)
            .await?
            .persisted_units
    };

    let stats = CompactStats {
        source_blocks,
        event_rows,
    };
    let note = compact_stats_note(&stats, &md_path, &event_log_path, overwrite);
    Ok(CompactOutcome {
        event_log_path,
        note,
        stats,
    })
}

/// Session-level stats comparing Vortex row count vs Markdown block count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerStats {
    pub event_rows: usize,
    pub markdown_blocks: usize,
    pub event_log_path: String,
    pub markdown_path: Option<String>,
    pub note: String,
}

pub async fn layer_stats(session: &TrajectorySession) -> Result<LayerStats> {
    let vortex = VortexTrajectoryStore;
    let markdown = MarkdownTrajectoryStore;

    let event_rows = if vortex.exists(session).await? {
        vortex.stats(session).await?.row_count
    } else {
        0
    };

    let (markdown_blocks, markdown_path) = if markdown.exists(session).await? {
        let run_dir = story_run_dir(
            &session.storage,
            &session.agent_id,
            &session.session_id,
            session.root_session_id.as_deref(),
        )?;
        let path = persisting_capture::markdown_trajectory::locate_session_markdown_for_key(
            &run_dir,
            &session.session_id,
        );
        let count = path
            .as_ref()
            .map(|p| persisting_capture::markdown_trajectory::block_count(p).unwrap_or(0))
            .unwrap_or(0);
        (count, path.map(|p| p.display().to_string()))
    } else {
        (0, None)
    };

    let note = if event_rows > 0 && markdown_blocks > 0 {
        format!(
            "Two-layer storage: Vortex {event_rows} raw event(s) ≥ Markdown {markdown_blocks} dialogue block(s)"
        )
    } else if event_rows > 0 {
        format!("Vortex only: {event_rows} raw event(s)")
    } else if markdown_blocks > 0 {
        format!("Markdown only: {markdown_blocks} block(s)")
    } else {
        "No trajectory data yet".to_string()
    };

    Ok(LayerStats {
        event_rows,
        markdown_blocks,
        event_log_path: vortex.display_path(session)?,
        markdown_path,
        note,
    })
}
