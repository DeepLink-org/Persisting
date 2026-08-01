//! Session / trace id extraction from HTTP headers (LiteLLM-compatible priority chain).

use axum::http::HeaderMap;

const EXPLICIT_SESSION_HEADERS: &[&str] = &[
    "x-persisting-trace-id",
    "x-litellm-trace-id",
    "x-litellm-session-id",
];

const GENERIC_SESSION_SUFFIX: &str = "-session-id";
const CLAUDE_AGENT_ID_HEADER: &str = "x-claude-code-agent-id";
const CLAUDE_PARENT_AGENT_ID_HEADER: &str = "x-claude-code-parent-agent-id";

/// Minimum length for vendor `x-*-session-id` values (UUIDs, hex ids, etc.).
const MIN_SESSION_ID_LEN: usize = 8;

/// Claude Code subagent instance id (`agent-{id}` in `~/.claude/projects/.../subagents/`).
pub fn extract_claude_agent_id(headers: &HeaderMap) -> Option<String> {
    header_value(headers, CLAUDE_AGENT_ID_HEADER)
}

/// Parent agent id when the request comes from a nested subagent spawn.
pub fn extract_claude_parent_agent_id(headers: &HeaderMap) -> Option<String> {
    header_value(headers, CLAUDE_PARENT_AGENT_ID_HEADER)
}

/// Session id from request headers only (no job fallback).
///
/// Priority:
/// 1. Configured `session_header` (e.g. `x-persisting-session-id`)
/// 2. `x-session-id`
/// 3. Explicit trace headers (`x-persisting-trace-id`, LiteLLM)
/// 4. Any `x-<vendor>-session-id` (e.g. Claude Code)
pub fn extract_session_from_headers(headers: &HeaderMap, session_header: &str) -> Option<String> {
    if let Some(v) = header_value(headers, session_header) {
        return Some(v);
    }
    if let Some(v) = header_value(headers, "x-session-id") {
        return Some(v);
    }
    for name in EXPLICIT_SESSION_HEADERS {
        if name.eq_ignore_ascii_case(session_header) {
            continue;
        }
        if let Some(v) = header_value(headers, name) {
            return Some(v);
        }
    }
    extract_generic_session_id(headers)
}

pub fn ephemeral_session_id() -> String {
    format!("ephemeral-{}", new_call_id().trim_start_matches("call-"))
}

/// Trace id for correlating request/response pairs and retries within a logical unit.
///
/// Falls back to `call_id` when no trace header is present.
pub fn resolve_trace_id(headers: &HeaderMap, call_id: &str) -> String {
    for name in [
        "x-persisting-trace-id",
        "x-litellm-trace-id",
        "x-request-id",
    ] {
        if let Some(v) = header_value(headers, name) {
            return v;
        }
    }
    call_id.to_string()
}

pub fn new_call_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("call-{ns:x}")
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn extract_generic_session_id(headers: &HeaderMap) -> Option<String> {
    for (name, value) in headers.iter() {
        let key = name.as_str();
        if !key.starts_with("x-") || !key.ends_with(GENERIC_SESSION_SUFFIX) {
            continue;
        }
        if EXPLICIT_SESSION_HEADERS
            .iter()
            .any(|h| key.eq_ignore_ascii_case(h))
        {
            continue;
        }
        if let Ok(v) = value.to_str() {
            let v = v.trim();
            if plausible_session_value(v) {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn plausible_session_value(v: &str) -> bool {
    if v.len() < MIN_SESSION_ID_LEN {
        return false;
    }
    v.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        use axum::http::header::HeaderName;
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).expect("header name"),
                HeaderValue::try_from(*v).expect("valid header value"),
            );
        }
        h
    }

    #[test]
    fn configured_session_header_wins() {
        let h = headers(&[
            ("x-persisting-session-id", "sess-a"),
            ("x-claude-code-session-id", "claude-b"),
        ]);
        assert_eq!(
            extract_session_from_headers(&h, "x-persisting-session-id").as_deref(),
            Some("sess-a")
        );
    }

    #[test]
    fn litellm_trace_id_used_when_no_configured_header() {
        let h = headers(&[("x-litellm-trace-id", "trace-12345678")]);
        assert_eq!(
            extract_session_from_headers(&h, "x-persisting-session-id").as_deref(),
            Some("trace-12345678")
        );
    }

    #[test]
    fn claude_code_session_id_from_header() {
        let h = headers(&[(
            "x-claude-code-session-id",
            "e96634a3-fa28-4083-b354-55542e2dca01",
        )]);
        assert_eq!(
            extract_session_from_headers(&h, "x-persisting-session-id").as_deref(),
            Some("e96634a3-fa28-4083-b354-55542e2dca01")
        );
    }

    #[test]
    fn short_generic_session_id_ignored() {
        let h = headers(&[("x-foo-session-id", "abc")]);
        assert!(extract_session_from_headers(&h, "x-persisting-session-id").is_none());
    }

    #[test]
    fn trace_id_from_header_or_call_id() {
        let h = headers(&[("x-persisting-trace-id", "trace-abc")]);
        assert_eq!(resolve_trace_id(&h, "call-deadbeef"), "trace-abc");
        assert_eq!(
            resolve_trace_id(&HeaderMap::new(), "call-deadbeef"),
            "call-deadbeef"
        );
    }

    #[test]
    fn claude_code_agent_id_from_header() {
        let h = headers(&[("x-claude-code-agent-id", "a5aa661")]);
        assert_eq!(extract_claude_agent_id(&h).as_deref(), Some("a5aa661"));
    }
}
