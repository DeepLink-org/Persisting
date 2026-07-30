//! Heuristic format detection from path or document content.

use std::path::Path;

use crate::format::ChronicleFormat;
use crate::Result;

/// Detect format from a file path (extension / basename).
///
/// `events` is detected only as a Lance dataset path (`events.lance`), never as `.json` / `.jsonl`.
pub fn detect_format_from_path(path: impl AsRef<Path>) -> Option<ChronicleFormat> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name == "storyline.json" || name.ends_with(".storyline.json") {
        return Some(ChronicleFormat::Storyline);
    }
    if name == "events.lance" || (name.ends_with(".lance") && name.contains("event")) {
        return Some(ChronicleFormat::Events);
    }
    if name == "session_steps.json" || name == "session_steps.lance" {
        return Some(ChronicleFormat::OpenaiMsg);
    }
    if name == "sessions.jsonl" || name == "steps.jsonl" || name == "tool_calls.jsonl" {
        return Some(ChronicleFormat::Atif);
    }
    if name.ends_with(".md") || name.ends_with(".tlv.md") {
        return Some(ChronicleFormat::Agenticmd);
    }
    None
}

/// Detect format from document text when path is unavailable.
///
/// Does **not** classify CaptureRecord-shaped JSON as `events` (Lance-only).
pub fn detect_format_from_content(input: &str) -> Result<Option<ChronicleFormat>> {
    let trimmed = input.trim_start();
    if trimmed.starts_with("---") && trimmed.contains("format: persisting:1.0") {
        return Ok(Some(ChronicleFormat::Agenticmd));
    }
    if trimmed.contains("<!-- persisting:block") {
        return Ok(Some(ChronicleFormat::Agenticmd));
    }
    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let spec = v
                .get("spec")
                .or_else(|| v.get("sv"))
                .or_else(|| v.get("schema_version"));
            if spec
                .and_then(|x| x.as_str())
                .is_some_and(|s| s.starts_with("storyline/"))
            {
                return Ok(Some(ChronicleFormat::Storyline));
            }
            if v.get("schema_version")
                .and_then(|x| x.as_str())
                .is_some_and(|s| s.starts_with("ATIF"))
            {
                return Ok(Some(ChronicleFormat::Atif));
            }
            if v.get("session_steps").is_some() || v.get("format_version").is_some() {
                return Ok(Some(ChronicleFormat::OpenaiMsg));
            }
            if v.get("turns").is_some()
                && (v.get("session").is_some()
                    || v.get("session_id").is_some()
                    || v.get("story_id").is_some())
            {
                return Ok(Some(ChronicleFormat::Storyline));
            }
            if v.get("steps").is_some() && v.get("agent").is_some() {
                return Ok(Some(ChronicleFormat::Atif));
            }
        }
    }
    if let Some(line) = trimmed.lines().find(|l| !l.trim().is_empty()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if v.get("session_id").is_some()
                && v.get("step_id").is_some()
                && v.get("agent_name").is_some()
            {
                return Ok(Some(ChronicleFormat::Atif));
            }
        }
    }
    Ok(None)
}

/// Prefer path detection; fall back to content.
pub fn detect_format(path: Option<&Path>, content: Option<&str>) -> Result<Option<ChronicleFormat>> {
    if let Some(p) = path {
        if let Some(fmt) = detect_format_from_path(p) {
            return Ok(Some(fmt));
        }
    }
    if let Some(c) = content {
        return detect_format_from_content(c);
    }
    Ok(None)
}
