//! Visible dialogue extraction from Claude Code Anthropic payloads.

use persisting_capture::dialogue_extract::{
    extract_assistant_turn_from_sse, extract_user_message_from_request_body,
    is_main_agent_continuation, push_sse_tool_snapshot, SseStreamBlockParser,
};

use super::support::fixture_body;

#[test]
fn skips_suggestion_mode_system_injection_in_request_extract() {
    let body = super::support::body_bytes(
        r#"{"messages":[{"role":"user","content":[{"type":"text","text":"[SUGGESTION MODE: predict next input]\nReply ONLY the suggestion."}]}]}"#,
    );
    assert!(extract_user_message_from_request_body(&body).is_none());
}

#[test]
fn extracts_user_turn_with_tool_result_blocks() {
    let body = fixture_body("agent_launch_tool_result.json");
    let text = extract_user_message_from_request_body(&body).expect("user text");
    assert!(text.contains("```tool_result:"));
    assert!(text.contains("agentId: a483eb5f3a45995b0"));
}

#[test]
fn unwraps_session_tag_wrapper_in_user_text() {
    let body = fixture_body("flash_session_probe.json");
    let text = extract_user_message_from_request_body(&body).expect("user text");
    assert_eq!(text, "Review design docs with subagents");
    assert!(!text.contains("<session>"));
}

#[test]
fn detects_task_notification_as_main_continuation() {
    let body = fixture_body("task_notification.json");
    assert!(is_main_agent_continuation(&body));
}

#[test]
fn subagent_explore_prompt_is_not_main_continuation() {
    let body = fixture_body("subagent_explore_flash.json");
    assert!(!is_main_agent_continuation(&body));
}

#[test]
fn sse_incremental_parser_emits_tool_agent_on_content_block_stop() {
    let sse = include_str!("fixtures/agent_tool_stream.sse");
    let mut parser = SseStreamBlockParser::default();
    let snap = push_sse_tool_snapshot(&mut parser, sse).expect("partial snapshot");
    assert!(snap.contains("```tool:Agent"));
    assert!(snap.contains("cli_capture_command.zh.md"));
}

#[test]
fn sse_visible_text_excludes_thinking_blocks() {
    let sse = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"secret"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}
"#;
    assert_eq!(extract_assistant_turn_from_sse(sse), "Hi");
}
