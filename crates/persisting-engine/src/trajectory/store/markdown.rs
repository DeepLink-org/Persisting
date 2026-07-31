//! Session markdown (`{session_id}.md`) trajectory backend.

use anyhow::{Context, Result};
use persisting_capture::markdown_trajectory::{append_engine_lines_to_markdown, replay_json_lines};
use persisting_pchronicle::{
    agenticmd_block_count, locate_session_markdown_for_key, read_agenticmd_blocks_from_file,
    sanitize_session_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
    SESSION_MARKDOWN_FILENAME,
};

use super::{
    TrajectoryAppendOutcome, TrajectoryReplayOutcome, TrajectorySession, TrajectoryStatsOutcome,
};
use crate::trajectory::trajectory_run_dir;

fn run_dir(session: &TrajectorySession) -> Result<std::path::PathBuf> {
    trajectory_run_dir(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )
}

pub fn display_path(session: &TrajectorySession) -> Result<String> {
    let run = run_dir(session)?;
    Ok(
        session_markdown_write_path_for_key(&run, &session.session_id)
            .display()
            .to_string(),
    )
}

pub fn exists(session: &TrajectorySession) -> Result<bool> {
    Ok(locate_session_markdown_for_key(&run_dir(session)?, &session.session_id).is_some())
}

pub fn append(session: &TrajectorySession, lines: &[String]) -> Result<TrajectoryAppendOutcome> {
    let accepted = lines.len();
    let run = run_dir(session)?;
    let md_path = session_markdown_write_path_for_key(&run, &session.session_id);
    let line_refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let n = append_engine_lines_to_markdown(&md_path, &line_refs)?;
    Ok(TrajectoryAppendOutcome {
        accepted_lines: accepted,
        persisted_units: n,
        note: format!("markdown: {} block(s) in {}", n, md_path.display()),
    })
}

pub fn replay(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<TrajectoryReplayOutcome> {
    let run = run_dir(session)?;
    let md_path = locate_session_markdown_for_key(&run, &session.session_id).ok_or_else(|| {
        anyhow::anyhow!(
            "markdown session file does not exist under {} (expected {}.md or legacy {})",
            run.display(),
            sanitize_session_filename(&session.session_id),
            SESSION_MARKDOWN_FILENAME
        )
    })?;
    let blocks = read_agenticmd_blocks_from_file(&md_path)?;
    let records = replay_json_lines(&blocks, offset, limit)?;
    Ok(TrajectoryReplayOutcome {
        records,
        note: format!(
            "Replay markdown {} ({} blocks), offset={}, limit={:?}.",
            md_path.display(),
            blocks.len(),
            offset,
            limit
        ),
    })
}

pub fn stats(session: &TrajectorySession) -> Result<TrajectoryStatsOutcome> {
    let run = run_dir(session)?;
    let default_path = session_markdown_path_for_key(&run, &session.session_id);
    let Some(md_path) = locate_session_markdown_for_key(&run, &session.session_id) else {
        return Ok(TrajectoryStatsOutcome {
            dataset: default_path.display().to_string(),
            row_count: 0,
            manifest_version: None,
            status: "missing".to_string(),
            note: format!(
                "No markdown file at {}; use trajectory add --storage markdown first.",
                default_path.display()
            ),
        });
    };
    let count = agenticmd_block_count(&md_path).context("markdown block_count")?;
    Ok(TrajectoryStatsOutcome {
        dataset: md_path.display().to_string(),
        row_count: count,
        manifest_version: None,
        status: "ok".to_string(),
        note: format!("markdown: {} block(s) in {}", count, md_path.display()),
    })
}
