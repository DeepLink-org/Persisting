//! Markdown trajectory filters for Claude Code traffic patterns.

use persisting_capture::config::CaptureLevel;
use persisting_capture::dialogue::skip_markdown_block;
use persisting_capture::engine::Call;
use persisting_capture::record::CaptureRecord;
use persisting_capture::sink::llm_request_summary_record;
use serde_json::json;

use super::support::fixture_body;

fn test_call() -> Call {
    Call {
        call_id: "call-test".into(),
        trace_id: "trace-test".into(),
        started_at: "2026-05-24T00:00:00Z".into(),
    }
}

fn request_record(
    model: &str,
    user: &str,
    subagent_id: Option<&str>,
    body: Option<&serde_json::Value>,
) -> CaptureRecord {
    let mut rec = llm_request_summary_record(
        Some(super::support::CLAUDE_SESSION.into()),
        Some(super::support::PROXY_AGENT.into()),
        model,
        "/v1/messages",
        100,
        "messages",
        "anthropic",
        Some(user.into()),
        None,
        &test_call(),
        CaptureLevel::Dialogue,
        body,
    );
    rec.subagent_id = subagent_id.map(str::to_string);
    if let Some(body) = body {
        if let Some(messages) = body.get("messages") {
            rec.payload["messages"] = messages.clone();
        }
        if let Some(model) = body.get("model").and_then(|m| m.as_str()) {
            rec.payload["model"] = json!(model);
        }
    }
    rec
}

#[test]
fn skips_main_session_flash_companion_before_pro_turn() {
    let rec = request_record(
        "deepseek-v4-flash",
        "再次开三个subagent，帮我看看设计文档的问题",
        None,
        None,
    );
    assert!(skip_markdown_block(&rec));
}

#[test]
fn keeps_main_session_pro_user_turn() {
    let rec = request_record(
        "deepseek-v4-pro",
        "再次开三个subagent，帮我看看设计文档的问题",
        None,
        None,
    );
    assert!(!skip_markdown_block(&rec));
}

#[test]
fn keeps_subagent_flash_explore_prompt() {
    let body: serde_json::Value =
        serde_json::from_slice(&fixture_body("subagent_explore_flash.json")).unwrap();
    let rec = request_record(
        "deepseek-v4-flash",
        "Review docs/src/design/cli_capture_command.zh.md",
        Some("ad20bef53147678d4"),
        Some(&body),
    );
    assert!(!skip_markdown_block(&rec));
}

#[test]
fn keeps_flash_session_title_probe_on_main() {
    let body: serde_json::Value =
        serde_json::from_slice(&fixture_body("flash_session_probe.json")).unwrap();
    let rec = request_record(
        "deepseek-v4-flash",
        "<session>\nReview design docs with subagents\n</session>",
        None,
        Some(&body),
    );
    assert!(!skip_markdown_block(&rec));
}

#[test]
fn skips_count_tokens_internal_requests() {
    let mut rec = request_record("deepseek-v4-pro", "huge context", None, None);
    rec.payload["path"] = json!("/v1/messages/count_tokens");
    assert!(skip_markdown_block(&rec));
}
