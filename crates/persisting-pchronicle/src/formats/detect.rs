//! Heuristic format detection from path or document content.

use std::path::Path;

use crate::Result;
use crate::format::DocumentFormat;
use crate::formats::registry;

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
    if let Some(format) = registry::detect(None, input.as_bytes())? {
        return Ok(Some(format));
    }
    Ok(detect_unregistered_from_content(input))
}

fn detect_unregistered_from_content(input: &str) -> Option<DocumentFormat> {
    let trimmed = input.trim_start();
    if trimmed.starts_with("---")
        && trimmed
            .lines()
            .any(|line| line.trim() == "format: persisting")
    {
        return Some(DocumentFormat::AgenticMd);
    }
    if trimmed.contains("<!-- persisting:block") {
        return Some(DocumentFormat::AgenticMd);
    }
    None
}

/// Prefer content fingerprints. Path names only fill in when content is insufficient.
pub fn detect_format(path: Option<&Path>, content: Option<&str>) -> Result<Option<DocumentFormat>> {
    if let Some(fmt) = registry::detect(path, content.map(str::as_bytes).unwrap_or(b""))? {
        return Ok(Some(fmt));
    }
    if let Some(c) = content
        && let Some(fmt) = detect_format_from_content(c)?
    {
        return Ok(Some(fmt));
    }
    if let Some(p) = path {
        return Ok(detect_format_from_path(p));
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
    fn does_not_guess_actf_from_steps_alone() {
        let input = r#"{
            "task_id":"travel-planning",
            "attempts":{"1":{
                "correct":false,
                "trajectory":{
                    "steps":[],
                    "started_at":"2026-06-17T07:26:27Z",
                    "finished_at":"2026-06-17T07:26:28Z"
                }
            }}
        }"#;
        assert_eq!(detect_format_from_content(input).unwrap(), None);
    }

    #[test]
    fn detects_actf_error_dump_with_empty_or_null_trajectory() {
        for trajectory in [r#"{}"#, "null"] {
            let input = format!(
                r#"{{
                    "task_id":"frontierscience_research_0053",
                    "category":"research",
                    "correct":false,
                    "attempts_tried":1,
                    "k":1,
                    "attempts":{{"1":{{
                        "correct":false,
                        "trajectory":{trajectory},
                        "meta":{{"status":"error","error":"TimeoutError: "}}
                    }}}}
                }}"#
            );
            assert_eq!(
                detect_format_from_content(&input).unwrap(),
                Some(DocumentFormat::Actf),
                "trajectory={trajectory}"
            );
        }
    }

    #[test]
    fn detects_actf_with_python_trajectory_repr_string() {
        let input = r#"{
            "task_id":"task_15_daily_summary",
            "category":"synthesis",
            "correct":false,
            "attempts_tried":1,
            "k":1,
            "attempts":{"1":{
                "correct":false,
                "trajectory":"Trajectory(schema_version='ACTF_v1.0', steps=[])"
            }}
        }"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Actf)
        );
    }

    #[test]
    fn detects_wireless_channel_actf_with_nan_observation_score() {
        // Frontier-engineering dumps emit Python NaN and put schema_version
        // after a large final_answer; full serde_json parse used to fail.
        let input = r#"{
            "task_id":"WirelessChannelSimulation/HighReliableSimulation",
            "category":"WirelessChannelSimulation",
            "correct":true,
            "solved_at":1,
            "attempts_tried":1,
            "k":1,
            "attempts":{"1":{
                "correct":true,
                "final_answer":"print(1)",
                "ground_truth":"",
                "trajectory":{
                    "schema_version":"ACTF_v1.0",
                    "steps":[{
                        "step_id":1,
                        "assistant_content":{"content":"iteration=0","reasoning_content":"","tool_calls":[]},
                        "metric":{"prompt_tokens_len":null,"completion_tokens_len":null,"llm_infer_ms":null,"env_action_ms":1.0,"stop_reason":null},
                        "system_prompt":"",
                        "user_content":"WirelessChannelSimulation/HighReliableSimulation",
                        "tools":[],
                        "observation":[{"combined_score": NaN}],
                        "started_at":"2026-01-01 00:00:00+00:00",
                        "finished_at":"2026-01-01 00:00:01+00:00"
                    }],
                    "started_at":"2026-01-01 00:00:00+00:00",
                    "finished_at":"2026-01-01 00:00:01+00:00"
                }
            }}
        }"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Actf)
        );
    }

    #[test]
    fn detects_atif_json_by_schema_and_agent_steps() {
        let versioned = r#"{"schema_version":"ATIF-v1.7","trajectory_id":"one","agent":{"name":"a","version":"1"},"steps":[]}"#;
        assert_eq!(
            detect_format_from_content(versioned).unwrap(),
            Some(DocumentFormat::Atif)
        );
        let unversioned = r#"{"session_id":"s","agent":{"name":"a","version":"1"},"steps":[]}"#;
        assert_eq!(
            detect_format_from_content(unversioned).unwrap(),
            Some(DocumentFormat::Atif)
        );
    }

    #[test]
    fn detects_codex_rollout_jsonl_by_name_and_first_line() {
        let input = r#"{"timestamp":"2026-08-03T08:15:11.000Z","type":"session_meta","payload":{"id":"019fc6b0-ec64-79d3-b51e-2a9d14fff365"}}
{"timestamp":"2026-08-03T08:15:12.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[]}}"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::Codex)
        );
        assert_eq!(
            detect_format(
                Some(Path::new(
                    "2026/08/03/rollout-2026-08-03T16-15-11-019fc6b0-ec64-79d3-b51e-2a9d14fff365.jsonl"
                )),
                None
            )
            .unwrap(),
            Some(DocumentFormat::Codex)
        );
    }

    #[test]
    fn content_fingerprint_overrides_rollout_path() {
        let claude = r#"{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(
            detect_format(Some(Path::new("rollout-mismatch.jsonl")), Some(claude)).unwrap(),
            Some(DocumentFormat::ClaudeCode)
        );
        assert_eq!(
            detect_format(Some(Path::new("rollout-empty.jsonl")), Some("\n")).unwrap(),
            Some(DocumentFormat::Codex)
        );
    }

    #[test]
    fn detects_claude_code_jsonl_and_does_not_steal_openai_rows() {
        let claude = r#"{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(
            detect_format_from_content(claude).unwrap(),
            Some(DocumentFormat::ClaudeCode)
        );
        let openai =
            r#"{"session_id":"s","step_id":1,"messages":[{"role":"user","content":"hi"}]}"#;
        assert_eq!(
            detect_format_from_content(openai).unwrap(),
            Some(DocumentFormat::OpenaiMsg)
        );
    }

    #[test]
    fn detects_claude_code_jsonl_after_non_transcript_preamble() {
        let input = r#"{"type":"mode","mode":"normal","sessionId":"sess-1"}
{"type":"file-history-snapshot","sessionId":"sess-1"}
{"type":"user","sessionId":"sess-1","uuid":"u1","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(
            detect_format_from_content(input).unwrap(),
            Some(DocumentFormat::ClaudeCode)
        );
    }
}
