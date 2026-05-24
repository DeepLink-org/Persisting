//! Shared helpers for IDE JSONL import providers (Claude, Cursor, …).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

pub fn since_cutoff(since_days: Option<u64>) -> Option<SystemTime> {
    since_days.map(|days| {
        SystemTime::now()
            .checked_sub(Duration::from_secs(days * 86400))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    })
}

pub fn project_dir_matches(dir_name: &str, project_filter: Option<&str>) -> bool {
    project_filter.is_none_or(|filter| dir_name.contains(filter))
}

pub fn path_matches_session(path: &Path, session_id: &str) -> bool {
    path.to_string_lossy().contains(session_id)
}

pub fn should_include_file(path: &Path, cutoff: Option<SystemTime>) -> bool {
    let Some(cutoff_time) = cutoff else {
        return true;
    };
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| mtime >= cutoff_time)
        .unwrap_or(true)
}

pub fn push_jsonl_file(
    path: PathBuf,
    cutoff: Option<SystemTime>,
    files: &mut Vec<(u64, PathBuf)>,
    file_order: &mut u64,
) {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return;
    }
    if should_include_file(&path, cutoff) {
        *file_order += 1;
        files.push((*file_order, path));
    }
}

pub fn collect_jsonl_recursive(
    dir: &Path,
    merge_subagents: bool,
    cutoff: Option<SystemTime>,
    files: &mut Vec<(u64, PathBuf)>,
    file_order: &mut u64,
) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "subagents" && !merge_subagents {
                continue;
            }
            collect_jsonl_recursive(&path, merge_subagents, cutoff, files, file_order)?;
            continue;
        }
        push_jsonl_file(path, cutoff, files, file_order);
    }
    Ok(())
}

pub fn parse_timestamp_nanos(ts: Option<&str>) -> Option<u64> {
    let ts = ts?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.timestamp_nanos_opt().unwrap_or(0) as u64)
}
