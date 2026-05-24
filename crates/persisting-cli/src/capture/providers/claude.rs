//! Claude Code session JSONL under `~/.claude/projects/`.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::capture::record::{PendingRecord, SortKey};

use super::ide_jsonl::{
    collect_jsonl_recursive, parse_timestamp_nanos, path_matches_session, project_dir_matches,
    since_cutoff,
};

const CLAUDE_PROJECTS: &str = ".claude/projects";

pub struct ClaudeOptions<'a> {
    pub project_filter: Option<&'a str>,
    pub since_days: Option<u64>,
    pub session_id: Option<&'a str>,
    pub merge_subagents: bool,
}

pub fn collect(opts: ClaudeOptions<'_>) -> Result<Vec<PendingRecord>> {
    let home = dirs::home_dir().context("home directory")?;
    let projects_dir = home.join(CLAUDE_PROJECTS);
    if !projects_dir.is_dir() {
        return Ok(Vec::new());
    }

    let cutoff = since_cutoff(opts.since_days);
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    let mut file_order: u64 = 0;

    for entry in
        fs::read_dir(&projects_dir).with_context(|| format!("read {}", projects_dir.display()))?
    {
        let entry = entry?;
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let dir_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if !project_dir_matches(dir_name, opts.project_filter) {
            continue;
        }
        collect_jsonl_recursive(
            &project_dir,
            opts.merge_subagents,
            cutoff,
            &mut files,
            &mut file_order,
        )?;
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = Vec::new();
    for (order, path) in files {
        if let Some(sid) = opts.session_id {
            if !path_matches_session(&path, sid) {
                continue;
            }
        }
        parse_file(&path, order, &mut out)?;
    }
    Ok(out)
}

fn parse_file(path: &Path, file_order: u64, out: &mut Vec<PendingRecord>) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);
    let session_from_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string);

    for (line_no, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = v
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("event")
            .to_string();
        let session_id = v
            .get("sessionId")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .or_else(|| session_from_name.clone());
        let agent_id = v
            .get("agentId")
            .and_then(|s| s.as_str())
            .map(str::to_string)
            .or_else(|| {
                path.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .filter(|n| *n == "subagents")
                    .and(path.file_stem())
                    .and_then(|s| s.to_str())
                    .map(|s| s.strip_prefix("agent-").unwrap_or(s).to_string())
            });
        let parent_uuid = v
            .get("parentUuid")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let timestamp = v
            .get("timestamp")
            .and_then(|s| s.as_str())
            .map(str::to_string);
        let ts_nanos = parse_timestamp_nanos(timestamp.as_deref()).unwrap_or(u64::MAX);

        out.push(PendingRecord {
            sort_key: SortKey {
                ts_nanos,
                file_order,
                line_no: line_no as u64,
            },
            source: "claude".to_string(),
            kind,
            timestamp,
            session_id,
            agent_id,
            parent_uuid,
            trace_id: None,
            payload: v,
        });
    }
    Ok(())
}
