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
    if name.ends_with(".actf.json") {
        return Some(ChronicleFormat::Actf);
    }
    if name.ends_with(".md") {
        return Some(ChronicleFormat::Agenticmd);
    }
    None
}

/// Detect format from document text when path is unavailable.
///
/// Does **not** classify EventRecord-shaped JSON as `events` (Lance-only).
pub fn detect_format_from_content(input: &str) -> Result<Option<ChronicleFormat>> {
    let trimmed = input.trim_start();
    if trimmed.starts_with("---")
        && trimmed
            .lines()
            .any(|line| line.trim() == "format: persisting")
    {
        return Ok(Some(ChronicleFormat::Agenticmd));
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return Ok(detect_json_format(&v));
        }

        // A JSONL/NDJSON corpus is not one JSON value. Inspect its first row
        // structurally before looking for text markers that may legitimately
        // occur inside a message payload.
        if let Some(line) = trimmed.lines().find(|line| !line.trim().is_empty()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                return Ok(detect_json_format(&v));
            }
        }
    }
    if trimmed.contains("<!-- persisting:block") {
        return Ok(Some(ChronicleFormat::Agenticmd));
    }
    Ok(None)
}

fn detect_json_format(v: &serde_json::Value) -> Option<ChronicleFormat> {
    let candidate = v.as_array().and_then(|values| values.first()).unwrap_or(v);
    let is_actf_document = v
        .get("attempts")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|attempts| {
            !attempts.is_empty()
                && attempts.values().all(|attempt| {
                    attempt
                        .get("trajectory")
                        .and_then(|trajectory| trajectory.get("schema_version"))
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|version| version.starts_with("ACTF_"))
                })
        });
    if is_actf_document {
        return Some(ChronicleFormat::Actf);
    }
    if candidate.get("session_id").is_some()
        && candidate.get("step_id").is_some()
        && ["messages", "messages_json", "response", "response_json"]
            .iter()
            .any(|field| candidate.get(*field).is_some())
    {
        return Some(ChronicleFormat::OpenaiMsg);
    }
    let spec = v
        .get("spec")
        .or_else(|| v.get("sv"))
        .or_else(|| v.get("schema_version"));
    if spec
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.starts_with("storyline/"))
    {
        return Some(ChronicleFormat::Storyline);
    }
    if v.get("schema_version")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.starts_with("ATIF"))
    {
        return Some(ChronicleFormat::Atif);
    }
    if v.get("session_steps").is_some() {
        return Some(ChronicleFormat::OpenaiMsg);
    }
    if v.get("turns").is_some()
        && (v.get("session").is_some()
            || v.get("session_id").is_some()
            || v.get("story_id").is_some())
    {
        return Some(ChronicleFormat::Storyline);
    }
    if v.get("steps").is_some() && v.get("agent").is_some() {
        return Some(ChronicleFormat::Atif);
    }
    if candidate
        .get("schema_version")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.starts_with("ATIF"))
        || (candidate.get("steps").is_some() && candidate.get("agent").is_some())
        || (candidate.get("session_id").is_some()
            && candidate.get("step_id").is_some()
            && candidate.get("agent_name").is_some())
    {
        return Some(ChronicleFormat::Atif);
    }
    None
}

/// Prefer path detection; fall back to content.
pub fn detect_format(
    path: Option<&Path>,
    content: Option<&str>,
) -> Result<Option<ChronicleFormat>> {
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
