//! Helpers for agentgateway LLM fixtures under `tests/fixtures/`.
//!
//! Fixture provenance and Apache-2.0 attribution: see `tests/fixtures/README.md`.

use std::fs;
use std::path::PathBuf;

use bytes::Bytes;
use serde_json::{json, Value};

/// Root of agentgateway LLM fixtures (`crates/persisting-capture/tests/fixtures/`).
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
    if raw.starts_with("---\n") {
        if let Some(end) = raw[4..].find("\n---\n") {
            return raw[4 + end + 5..].trim_start();
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

/// AG streaming snaps are SSE text after frontmatter.
pub fn parse_ag_sse_snap(relative: &str) -> String {
    strip_snap_frontmatter(&read_fixture(relative)).to_string()
}

/// Normalize completions request JSON for comparison with AG `.completions.snap`.
pub fn normalize_completions_request(v: &mut Value) {
    let Some(obj) = v.as_object_mut() else {
        return;
    };
    obj.remove("stream");
    if obj.contains_key("max_tokens") && !obj.contains_key("max_completion_tokens") {
        if let Some(mt) = obj.remove("max_tokens") {
            obj.insert("max_completion_tokens".to_string(), mt);
        }
    }
    if let Some(msgs) = obj.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in msgs.iter_mut() {
            if let Some(content) = msg.get_mut("content") {
                *content = normalize_message_content(content.clone());
            }
        }
    }
}

fn normalize_message_content(v: Value) -> Value {
    match v {
        Value::String(s) => json!([{"type": "text", "text": s}]),
        Value::Object(ref obj) if obj.get("type").is_some() => json!([v]),
        other => other,
    }
}

/// Normalize Anthropic message response for comparison with AG snaps.
pub fn normalize_messages_response(v: &mut Value) {
    if let Some(id) = v.get_mut("id") {
        *id = json!("[id]");
    }
    if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
        // AG snaps keep upstream model from completions response; client_model may differ.
        let _ = model;
    }
}

pub fn assert_json_eq(actual: &Value, expected: &Value, context: &str) {
    let mut a = actual.clone();
    let mut e = expected.clone();
    normalize_completions_request(&mut a);
    normalize_completions_request(&mut e);
    // Persisting intentionally strips tools/thinking on messages→completions (minimal bridge).
    if let Some(ao) = a.as_object_mut() {
        ao.remove("tools");
        ao.remove("output_format");
    }
    if let Some(eo) = e.as_object_mut() {
        eo.remove("tools");
        eo.remove("reasoning_effort");
    }
    assert_eq!(a, e, "{context}");
}

pub fn assert_messages_response_eq(actual: &Value, expected: &Value, context: &str) {
    let mut a = actual.clone();
    let mut e = expected.clone();
    normalize_messages_response(&mut a);
    normalize_messages_response(&mut e);
    assert_eq!(a["type"], e["type"], "{context}: type");
    assert_eq!(a["role"], e["role"], "{context}: role");
    assert_eq!(a["content"], e["content"], "{context}: content");
    assert_eq!(a["stop_reason"], e["stop_reason"], "{context}: stop_reason");
    if e.get("usage").is_some() {
        assert_eq!(
            a["usage"]["input_tokens"], e["usage"]["input_tokens"],
            "{context}: input_tokens"
        );
        assert_eq!(
            a["usage"]["output_tokens"], e["usage"]["output_tokens"],
            "{context}: output_tokens"
        );
    }
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

pub fn fixture_exists(relative: &str) -> bool {
    fixture_path(relative).is_file()
}

/// Case tables aligned with agentgateway `llm/tests.rs` (Persisting-supported bridges only).
pub const MESSAGES_TO_COMPLETIONS: &[&str] = &["basic", "tools", "reasoning"];

pub const COMPLETIONS_TO_MESSAGES: &[&str] = &["basic"];

pub const RESPONSES_TO_COMPLETIONS: &[&str] = &["basic", "instructions", "input-list"];

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
        let v = parse_ag_json_snap("requests/messages/basic.completions.snap");
        assert!(v.get("messages").is_some());
    }
}
