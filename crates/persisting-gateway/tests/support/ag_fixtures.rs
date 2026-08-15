//! Helpers for agentgateway LLM fixtures under `tests/fixtures/`.
//!
//! Fixture provenance and Apache-2.0 attribution: see `tests/fixtures/README.md`.

use std::fs;
use std::path::PathBuf;

use bytes::Bytes;
use serde_json::{json, Value};

/// Root of agentgateway LLM fixtures (`crates/persisting-gateway/tests/fixtures/`).
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn fixture_path(relative: &str) -> PathBuf {
    fixtures_root().join(relative)
}

pub fn read_fixture(relative: &str) -> String {
    fs::read_to_string(fixture_path(relative))
        .unwrap_or_else(|e| panic!("read fixture {relative}: {e}"))
}

pub fn read_fixture_bytes(relative: &str) -> Bytes {
    Bytes::from(
        fs::read(fixture_path(relative))
            .unwrap_or_else(|e| panic!("read fixture bytes {relative}: {e}")),
    )
}

/// Strip insta YAML frontmatter (`---` … `---`) from AG snapshot files.
pub fn strip_snap_frontmatter(raw: &str) -> &str {
    if let Some(stripped) = raw.strip_prefix("---\n") {
        if let Some(end) = stripped.find("\n---\n") {
            return stripped[end + 5..].trim_start();
        }
    }
    raw.trim()
}

/// Parse JSON body from an AG `.snap` file (after frontmatter).
pub fn parse_ag_json_snap(relative: &str) -> Value {
    let raw = read_fixture(relative);
    let body = strip_snap_frontmatter(&raw);
    serde_json::from_str(body).unwrap_or_else(|e| panic!("parse snap JSON {relative}: {e}"))
}

/// AG response snaps wrap wire output in `{ "response": ..., "parsed": ... }`.
pub fn ag_snap_response(relative: &str) -> Value {
    let v = parse_ag_json_snap(relative);
    v.get("response")
        .cloned()
        .unwrap_or_else(|| panic!("snap {relative} missing .response"))
}

/// AG request snaps wrap wire output in `{ "request": ..., "parsed": ... }`.
pub fn ag_snap_request(relative: &str) -> Value {
    let value = parse_ag_json_snap(relative);
    value.get("request").cloned().unwrap_or(value)
}

/// AG streaming snaps are SSE text after frontmatter.
pub fn parse_ag_sse_snap(relative: &str) -> String {
    strip_snap_frontmatter(&read_fixture(relative)).to_string()
}

/// Normalize Anthropic message response for comparison with AG snaps.
pub fn normalize_messages_response(v: &mut Value) {
    if let Some(id) = v.get_mut("id") {
        *id = json!("[id]");
    }
    if let Some(content) = v.get_mut("content").and_then(|c| c.as_array_mut()) {
        // Persisting bridge emits an empty text block when upstream choice has no content.
        if content.len() == 1
            && content[0].get("type").and_then(|t| t.as_str()) == Some("text")
            && content[0]
                .get("text")
                .and_then(|t| t.as_str())
                .is_some_and(str::is_empty)
        {
            content.clear();
        }
    }
    if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
        // AG snaps keep upstream model from completions response; client_model may differ.
        let _ = model;
    }
}

pub fn assert_json_eq(actual: &Value, expected: &Value, context: &str) {
    assert_eq!(actual, expected, "{context}");
}

pub fn assert_messages_response_eq(actual: &Value, expected: &Value, context: &str) {
    let mut a = actual.clone();
    let mut e = expected.clone();
    normalize_messages_response(&mut a);
    normalize_messages_response(&mut e);
    assert_eq!(a, e, "{context}");
}

/// Feed full upstream OpenAI SSE fixture through a translator callback.
pub fn translate_openai_sse_fixture<F>(relative: &str, mut translate: F) -> String
where
    F: FnMut(&[u8]) -> anyhow::Result<String>,
{
    let raw = read_fixture(relative);
    let mut out = String::new();
    for chunk in raw.as_bytes().chunks(512) {
        out.push_str(&translate(chunk).expect("translate SSE chunk"));
    }
    out
}

pub fn sse_event_names(sse: &str) -> Vec<&str> {
    sse.lines()
        .filter(|l| l.starts_with("event: "))
        .map(|l| l.strip_prefix("event: ").unwrap_or(""))
        .collect()
}

/// Parse SSE into protocol events, ignoring agentgateway's trailing parsed-info JSON.
pub fn parse_sse_events(sse: &str) -> Vec<(String, Value)> {
    let mut events = Vec::new();
    let mut event_name = None;
    for line in sse.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            event_name = Some(name.to_string());
        } else if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                continue;
            }
            if let (Some(name), Ok(value)) = (event_name.take(), serde_json::from_str(data)) {
                events.push((name, value));
            }
        }
    }
    events
}

pub fn fixture_exists(relative: &str) -> bool {
    fixture_path(relative).is_file()
}

/// Case tables aligned with agentgateway `llm/tests.rs` (Persisting-supported bridges only).
pub const MESSAGES_TO_COMPLETIONS: &[&str] = &[
    "basic",
    "cache_control",
    "gpt_adaptive_thinking_with_tools",
    "metadata",
    "reasoning",
    "server_tools",
    "structured-output",
    "system_message",
    "tools",
];

pub const COMPLETIONS_TO_MESSAGES: &[&str] = &[
    "basic",
    "cache_write",
    "gemini_with_completion_tokens",
    "gemini_zero_completion_tokens",
    "openrouter_reasoning",
    "audio",
    "tool_call",
    "truncated_tool_call",
];

pub const RESPONSES_TO_COMPLETIONS: &[&str] = &[
    "basic",
    "instructions",
    "input-list",
    "assistant-history",
    "parallel-tool-call",
];

/// Gemini native `generateContent` request goldens. The upstream corpus names the shared
/// Google wire contract `vertex-gemini`; Gemini API uses the same request/response schema.
pub const COMPLETIONS_TO_GEMINI: &[&str] = &[
    "basic",
    "generation-config",
    "image-file",
    "image-inline",
    "multi-turn-tools",
    "parallel-tool-call",
    "reasoning",
    "reasoning_max",
    "structured-output",
    "tool-call",
];

pub const GEMINI_TO_COMPLETIONS: &[&str] = &["basic", "tool", "reasoning", "blocked"];

/// Tracks how many fixture cases actually ran (guards against silent `continue` on missing files).
#[derive(Debug, Default, Clone, Copy)]
pub struct CaseReport {
    pub ran: usize,
    pub skipped: usize,
}

impl CaseReport {
    pub fn record_ran(&mut self) {
        self.ran += 1;
    }

    pub fn record_skipped(&mut self) {
        self.skipped += 1;
    }

    pub fn assert_min_ran(&self, min: usize, label: &str) {
        assert!(
            self.ran >= min,
            "{label}: expected >= {min} cases, ran {} skipped {}",
            self.ran,
            self.skipped
        );
    }
}

pub fn load_json_fixture(relative: &str) -> Value {
    serde_json::from_slice(&read_fixture_bytes(relative))
        .unwrap_or_else(|e| panic!("parse JSON fixture {relative}: {e}"))
}

pub fn for_each_existing(relative_paths: &[&str], mut f: impl FnMut(&str)) -> CaseReport {
    let mut report = CaseReport::default();
    for path in relative_paths {
        if fixture_exists(path) {
            f(path);
            report.record_ran();
        } else {
            report.record_skipped();
        }
    }
    report
}

pub fn for_each_existing_case(
    cases: &[&str],
    prefix: &str,
    suffix: &str,
    mut f: impl FnMut(&str),
) -> CaseReport {
    let mut report = CaseReport::default();
    for case in cases {
        let path = format!("{prefix}{case}{suffix}");
        if fixture_exists(&path) {
            f(case);
            report.record_ran();
        } else {
            report.record_skipped();
        }
    }
    report
}

pub fn messages_completions_snap(case: &str) -> String {
    format!("requests/messages/{case}.completions.snap")
}

pub fn completions_messages_snap(case: &str) -> String {
    format!("response/completions/{case}.completions-messages.snap")
}

pub fn upstream_model_from_messages_fixture(case: &str) -> String {
    let v: Value = serde_json::from_str(&read_fixture(&format!("requests/messages/{case}.json")))
        .expect("parse messages fixture");
    v.get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("upstream-model")
        .to_string()
}

pub fn client_model_from_completions_fixture(case: &str) -> String {
    let v: Value =
        serde_json::from_str(&read_fixture(&format!("response/completions/{case}.json")))
            .expect("parse completions fixture");
    v.get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("client-model")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_root_exists() {
        assert!(fixtures_root().is_dir());
    }

    #[test]
    fn parse_basic_completions_snap() {
        let v = ag_snap_request("requests/messages/basic.completions.snap");
        assert!(v.get("messages").is_some());
    }
}
