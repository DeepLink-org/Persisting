//! Heuristic format detection from path or document content.

use std::path::Path;

use crate::format::DocumentFormat;
use crate::Result;

/// Detect format from a file path (extension / basename).
///
/// `events` is detected only as a Lance dataset path (`events.lance`), never as `.json` / `.jsonl`.
pub fn detect_format_from_path(path: impl AsRef<Path>) -> Option<DocumentFormat> {
    let path = path.as_ref();
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if name == "events.lance" || (name.ends_with(".lance") && name.contains("event")) {
        return Some(DocumentFormat::CanonicalEvent);
    }
    if name == "session_steps.json" {
        return Some(DocumentFormat::OpenaiMsg);
    }
    if name.ends_with(".storyline.json") {
        return Some(DocumentFormat::Storyline);
    }
    if name.ends_with(".actf.json") {
        return Some(DocumentFormat::Actf);
    }
    if name.ends_with(".md") {
        return Some(DocumentFormat::AgenticMd);
    }
    None
}

/// Detect format from document text when path is unavailable.
///
/// Does **not** classify EventRecord-shaped JSON as `events` (Lance-only).
pub fn detect_format_from_content(input: &str) -> Result<Option<DocumentFormat>> {
    let trimmed = input.trim_start();
    if trimmed.starts_with("---")
        && trimmed
            .lines()
            .any(|line| line.trim() == "format: persisting")
    {
        return Ok(Some(DocumentFormat::AgenticMd));
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
        return Ok(Some(DocumentFormat::AgenticMd));
    }
    Ok(None)
}

fn looks_like_actf_attempt(attempt: &serde_json::Value) -> bool {
    match attempt.get("trajectory") {
        Some(trajectory) if trajectory.is_array() => trajectory
            .as_array()
            .is_some_and(|events| events.iter().all(serde_json::Value::is_object)),
        Some(trajectory) => trajectory
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|version| version.starts_with("ACTF_")),
        None => false,
    }
}

fn looks_like_actf_document(v: &serde_json::Value) -> bool {
    v.get("task_id").is_some()
        && v.get("attempts")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|attempts| {
                !attempts.is_empty() && attempts.values().all(looks_like_actf_attempt)
            })
}

fn detect_json_format(v: &serde_json::Value) -> Option<DocumentFormat> {
    let candidate = v.as_array().and_then(|values| values.first()).unwrap_or(v);
    if candidate
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|version| version.starts_with("storyline/"))
    {
        return Some(DocumentFormat::Storyline);
    }
    if looks_like_actf_document(v)
        || v.as_array()
            .is_some_and(|values| !values.is_empty() && values.iter().all(looks_like_actf_document))
    {
        return Some(DocumentFormat::Actf);
    }
    if candidate.get("session_id").is_some()
        && candidate.get("step_id").is_some()
        && ["messages", "messages_json", "response", "response_json"]
            .iter()
            .any(|field| candidate.get(*field).is_some())
    {
        return Some(DocumentFormat::OpenaiMsg);
    }
    if v.get("schema_version")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.starts_with("ATIF"))
    {
        return Some(DocumentFormat::Atif);
    }
    if v.get("session_steps").is_some() {
        return Some(DocumentFormat::OpenaiMsg);
    }
    if v.get("steps").is_some() && v.get("agent").is_some() {
        return Some(DocumentFormat::Atif);
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
        return Some(DocumentFormat::Atif);
    }
    None
}

/// Prefer path detection; fall back to content.
pub fn detect_format(path: Option<&Path>, content: Option<&str>) -> Result<Option<DocumentFormat>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_storyline_json_by_version_and_specific_suffix() {
        let input =
            r#"{"schema_version":"storyline/v1","session":"s","agent":{"id":"a"},"turns":[]}"#;

        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Storyline)
        );
        assert_eq!(
            detect_format_from_path("trajectory.storyline.json"),
            Some(DocumentFormat::Storyline)
        );
    }

    #[test]
    fn detects_actf_error_dump_with_event_log_trajectory() {
        let input = r#"{
            "task_id":"gravitational-wave-detection",
            "category":"astronomy",
            "k":1,
            "correct":false,
            "attempts_tried":1,
            "attempts":{"1":{
                "correct":false,
                "status":"run_error",
                "trajectory":[
                    {"type":"session","id":"s1","timestamp":"2026-06-17T07:26:27.170Z","cwd":"/root"},
                    {"type":"message","id":"m1","timestamp":"2026-06-17T07:26:28Z",
                     "message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
                ]
            }}
        }"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Actf)
        );
    }

    #[test]
    fn detects_array_of_actf_documents() {
        let input = r#"[{"task_id":"a","category":"test","k":1,"correct":false,"attempts_tried":1,"attempts":{"1":{"trajectory":{"schema_version":"ACTF_v1.0","steps":[]}}}},{"task_id":"b","category":"test","k":1,"correct":false,"attempts_tried":1,"attempts":{"1":{"trajectory":{"schema_version":"ACTF_v1.0","steps":[]}}}}]"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Actf)
        );
    }
}
