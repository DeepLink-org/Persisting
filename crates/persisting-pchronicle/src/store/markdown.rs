//! AgenticMD session projection backend.

use anyhow::{Context, Result};
use std::collections::HashSet;

use super::{AppendOutcome, ReplayOutcome, TrajectorySession, TrajectoryStats};
use crate::{
    agenticmd_block_count, agenticmd_block_to_replay_json, append_agenticmd_blocks,
    event_record_to_agenticmd_block, locate_session_markdown_for_key,
    read_agenticmd_blocks_from_file, sanitize_session_filename, session_markdown_path_for_key,
    session_markdown_write_path_for_key, EventRecord, SESSION_MARKDOWN_FILENAME,
};

fn run_dir(session: &TrajectorySession) -> Result<std::path::PathBuf> {
    session.run_dir()
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

pub fn append(session: &TrajectorySession, lines: &[String]) -> Result<AppendOutcome> {
    let accepted = lines.len();
    let run = run_dir(session)?;
    let md_path = session_markdown_write_path_for_key(&run, &session.session_id);
    let records = crate::decode_event_lines(lines)?;
    let blocks = project_dialogue_blocks(&records)?;
    let n = append_agenticmd_blocks(&md_path, &blocks, None)?;
    Ok(AppendOutcome {
        accepted_records: accepted,
        persisted_units: n,
        note: format!("markdown: {} block(s) in {}", n, md_path.display()),
    })
}

pub fn replay(
    session: &TrajectorySession,
    offset: usize,
    limit: Option<usize>,
) -> Result<ReplayOutcome> {
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
    let end = limit
        .map(|lim| offset.saturating_add(lim).min(blocks.len()))
        .unwrap_or(blocks.len());
    let records = blocks
        .get(offset..end)
        .unwrap_or(&[])
        .iter()
        .map(agenticmd_block_to_replay_json)
        .collect::<Result<Vec<_>>>()?;
    Ok(ReplayOutcome {
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

pub fn stats(session: &TrajectorySession) -> Result<TrajectoryStats> {
    let run = run_dir(session)?;
    let default_path = session_markdown_path_for_key(&run, &session.session_id);
    let Some(md_path) = locate_session_markdown_for_key(&run, &session.session_id) else {
        return Ok(TrajectoryStats {
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
    Ok(TrajectoryStats {
        dataset: md_path.display().to_string(),
        row_count: count,
        manifest_version: None,
        status: "ok".to_string(),
        note: format!("markdown: {} block(s) in {}", count, md_path.display()),
    })
}

/// Stateful event → dialogue projection shared by Markdown storage and materialization.
pub(crate) fn project_dialogue_blocks(
    records: &[EventRecord],
) -> Result<Vec<crate::AgenticmdBlock>> {
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
            let block = event_record_to_agenticmd_block(record)?;
            if !block.body.trim().is_empty() || record.kind == "llm.spawn_link" {
                blocks.push(block);
            }
        }
    }
    Ok(blocks)
}
