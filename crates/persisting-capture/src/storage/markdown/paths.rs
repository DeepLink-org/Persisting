//! Session markdown path resolution and filename rules.

use std::path::{Path, PathBuf};

use super::types::{
    LEGACY_TRAJECTORY_MARKDOWN_FILENAME, SESSION_FILENAME_MAX_LEN, SESSION_MARKDOWN_FILENAME,
};

/// Sanitize a logical session id for use as a markdown filename stem.
pub fn sanitize_session_filename(session_id: &str) -> String {
    let trimmed = session_id.trim();
    let mut out = String::with_capacity(trimmed.len().min(SESSION_FILENAME_MAX_LEN));
    for c in trimmed.chars() {
        if out.len() >= SESSION_FILENAME_MAX_LEN {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_string()
    } else {
        out
    }
}

/// Markdown filename for a logical session (`{session_id}.md`).
pub fn session_markdown_filename(session_key: &str) -> String {
    format!("{}.md", sanitize_session_filename(session_key))
}

pub fn session_markdown_path(session_dir: &Path) -> PathBuf {
    session_dir.join(SESSION_MARKDOWN_FILENAME)
}

/// `{run_dir}/{session_key}.md`
pub fn session_markdown_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    run_dir.join(session_markdown_filename(session_key))
}

/// Path to append markdown blocks for one session key under a run directory.
pub fn session_markdown_write_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    locate_session_markdown_for_key(run_dir, session_key)
        .unwrap_or_else(|| session_markdown_path_for_key(run_dir, session_key))
}

/// Path to append markdown blocks: existing session file, or [`SESSION_MARKDOWN_FILENAME`] for new sessions.
pub fn session_markdown_write_path(session_dir: &Path) -> PathBuf {
    locate_session_markdown(session_dir).unwrap_or_else(|| session_markdown_path(session_dir))
}

/// Whether `session_key` names a subagent markdown stem (`agent-{id}`).
pub fn is_subagent_session_storage_key(session_key: &str) -> bool {
    sanitize_session_filename(session_key)
        .strip_prefix("agent-")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn is_subagent_markdown_filename(name: &str) -> bool {
    name.strip_suffix(".md")
        .is_some_and(|stem| is_subagent_session_storage_key(stem))
}

/// Capture-run bucket markdown: `{run_dir}/run-{id}.md` when the directory itself is `run-*`.
pub fn locate_run_bucket_markdown(run_dir: &Path) -> Option<PathBuf> {
    let stem = run_dir.file_name()?.to_str()?;
    if !stem.starts_with("run-") {
        return None;
    }
    let path = run_dir.join(session_markdown_filename(stem));
    path.is_file().then_some(path)
}

/// Find markdown for one session key under a run directory (prefers `{key}.md`, then legacy `0001.md`).
pub fn locate_session_markdown_for_key(run_dir: &Path, session_key: &str) -> Option<PathBuf> {
    let named = session_markdown_path_for_key(run_dir, session_key);
    if named.is_file() {
        return Some(named);
    }
    // Subagent keys always map to sibling `agent-{id}.md`; never inherit the main run file.
    if is_subagent_session_storage_key(session_key) {
        return None;
    }
    // Main session in a capture run: prefer the run bucket file over subagent siblings.
    if let Some(run_md) = locate_run_bucket_markdown(run_dir) {
        return Some(run_md);
    }
    locate_session_markdown(run_dir)
}

/// Find the readable session markdown file (legacy `0001.md`, `{session_id}.md`, …).
pub fn locate_session_markdown(session_dir: &Path) -> Option<PathBuf> {
    let preferred = session_dir.join(SESSION_MARKDOWN_FILENAME);
    if preferred.is_file() {
        return Some(preferred);
    }
    let legacy = session_dir.join(LEGACY_TRAJECTORY_MARKDOWN_FILENAME);
    if legacy.is_file() {
        return Some(legacy);
    }
    let mut session_key_files = Vec::new();
    let mut numbered = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(session_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if is_numbered_session_markdown_name(name) {
                numbered.push(path);
            } else if is_session_key_markdown_name(name) && !is_subagent_markdown_filename(name) {
                session_key_files.push(path);
            }
        }
    }
    session_key_files.sort();
    if let Some(path) = session_key_files.into_iter().next() {
        return Some(path);
    }
    numbered.sort();
    numbered.into_iter().next()
}

/// Whether `path` names a persisting session markdown file (`{session_id}.md`, `0001.md`, …).
pub fn is_trajectory_markdown_path(path: impl AsRef<Path>) -> bool {
    let Some(name) = path.as_ref().file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    is_numbered_session_markdown_name(name)
        || is_session_key_markdown_name(name)
        || name.eq_ignore_ascii_case(LEGACY_TRAJECTORY_MARKDOWN_FILENAME)
        || name.to_ascii_lowercase().ends_with(".tlv.md")
}

fn is_session_key_markdown_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    if stem.is_empty()
        || is_numbered_session_markdown_name(name)
        || name.eq_ignore_ascii_case(LEGACY_TRAJECTORY_MARKDOWN_FILENAME)
        || name.to_ascii_lowercase().ends_with(".tlv.md")
    {
        return false;
    }
    stem.starts_with("agent-")
        || stem.starts_with("run-")
        || (stem.contains('-') && stem.len() >= 8)
}

fn is_numbered_session_markdown_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    stem.len() == 4 && stem.chars().all(|c| c.is_ascii_digit())
}
