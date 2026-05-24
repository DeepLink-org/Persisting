//! Cursor agent transcripts under `~/.cursor/projects/.../agent-transcripts/`.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::capture::record::{PendingRecord, SortKey};

use super::ide_jsonl::{
    parse_timestamp_nanos, path_matches_session, project_dir_matches, push_jsonl_file, since_cutoff,
};

const CURSOR_PROJECTS: &str = ".cursor/projects";

pub struct CursorOptions<'a> {
    pub project_filter: Option<&'a str>,
    pub since_days: Option<u64>,
    pub session_id: Option<&'a str>,
    pub merge_subagents: bool,
}

pub fn collect(opts: CursorOptions<'_>) -> Result<Vec<PendingRecord>> {
    let home = dirs::home_dir().context("home directory")?;
    let projects_dir = home.join(CURSOR_PROJECTS);
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
        let transcripts = project_dir.join("agent-transcripts");
        if !transcripts.is_dir() {
            continue;
        }
        collect_transcripts(
            &transcripts,
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

fn collect_transcripts(
    dir: &Path,
    merge_subagents: bool,
    cutoff: Option<std::time::SystemTime>,
    files: &mut Vec<(u64, PathBuf)>,
    file_order: &mut u64,
) -> Result<()> {
    for session_entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let session_entry = session_entry?;
        let session_dir = session_entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        for entry in
            fs::read_dir(&session_dir).with_context(|| format!("read {}", session_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "subagents" && merge_subagents {
                    collect_jsonl_in_dir(&path, cutoff, files, file_order)?;
                }
                continue;
            }
            push_jsonl_file(path, cutoff, files, file_order);
        }
    }
    Ok(())
}

fn collect_jsonl_in_dir(
    dir: &Path,
    cutoff: Option<std::time::SystemTime>,
    files: &mut Vec<(u64, PathBuf)>,
    file_order: &mut u64,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        push_jsonl_file(entry.path(), cutoff, files, file_order);
    }
    Ok(())
}

fn parse_file(path: &Path, file_order: u64, out: &mut Vec<PendingRecord>) -> Result<()> {
    let session_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(str::to_string);

    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let reader = BufReader::new(file);

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
            .or_else(|| v.get("role").and_then(|t| t.as_str()))
            .unwrap_or("event")
            .to_string();
        let agent_id = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|n| *n == "subagents")
            .and(path.file_stem())
            .and_then(|s| s.to_str())
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
            source: "cursor".to_string(),
            kind,
            timestamp,
            session_id: session_id.clone(),
            agent_id,
            parent_uuid: None,
            trace_id: None,
            payload: v,
        });
    }
    Ok(())
}
