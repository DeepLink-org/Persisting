use serde_json::{json, Value};

use super::*;
use crate::config::CaptureLevel;
use crate::record::CaptureRecordExt;
use crate::sink::{
    llm_request_record, llm_request_summary_record, llm_response_record,
    llm_response_record_with_content,
};
use crate::Call;
use persisting_pchronicle::agenticmd_block_to_replay_json;
use persisting_pchronicle::{
    encode_agenticmd_block_validated, parse_agenticmd_document_validated as parse_document,
};
fn test_call() -> Call {
    Call {
        call_id: "call-test".into(),
        trace_id: "trace-test".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    }
}

const LEVEL: CaptureLevel = CaptureLevel::Dialogue;

#[test]
fn proxy_nested_body_writes_plain_content() {
    let req = llm_request_record(
        Some("sess".into()),
        Some("agent".into()),
        "mock-model",
        "/v1/chat/completions",
        &json!({
            "protocol": "chat_completions",
            "provider": "openai",
            "body": {"messages":[{"role":"user","content":"你好"}],"model":"mock-model"},
        }),
    );
    let resp = llm_response_record(
        Some("sess".into()),
        Some("agent".into()),
        200,
        &json!({
            "protocol": "chat_completions",
            "provider": "openai",
            "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
            "body": {
                "choices":[{"message":{"role":"assistant","content":"你好！"}}],
            },
        }),
        false,
        &test_call(),
    );
    let b1 = capture_record_to_agenticmd_block(&req).unwrap();
    let b2 = capture_record_to_agenticmd_block(&resp).unwrap();
    assert_eq!(b1.body, "你好");
    assert_eq!(b2.body, "你好！");
}

#[test]
fn llm_pair_writes_and_replays() {
    let req = llm_request_record(
        Some("sess".into()),
        None,
        "deepseek-chat",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"你好"}]}),
    );
    let resp = llm_response_record(
        Some("sess".into()),
        None,
        200,
        &json!({
            "choices":[{"message":{"role":"assistant","content":"你好！"}}],
            "usage":{"prompt_tokens":12,"completion_tokens":18,"total_tokens":30}
        }),
        false,
        &test_call(),
    );
    let b1 = capture_record_to_agenticmd_block(&req).unwrap();
    let b2 = capture_record_to_agenticmd_block(&resp).unwrap();
    let doc = format!(
        "{}{}",
        encode_agenticmd_block_validated(&b1).unwrap(),
        encode_agenticmd_block_validated(&b2).unwrap(),
    );
    let blocks = parse_document(&doc).unwrap();
    let row: Value =
        serde_json::from_str(&agenticmd_block_to_replay_json(&blocks[0]).unwrap()).unwrap();
    assert_eq!(row["role"], "user");
    assert_eq!(row["content"], "你好");
}

#[test]
fn summary_request_writes_user_content_not_json() {
    let req = llm_request_summary_record(
        Some("sess".into()),
        Some("agent".into()),
        "deepseek-v4-pro",
        "/v1/messages",
        111_027,
        "messages",
        "anthropic",
        Some("hi".into()),
        None,
        &test_call(),
        LEVEL,
        None,
    );
    let block = capture_record_to_agenticmd_block(&req).unwrap();
    assert_eq!(block.body, "hi");
}

#[test]
fn stream_response_with_assistant_content_writes_plain_text() {
    let resp = llm_response_record_with_content(
        Some("sess".into()),
        Some("agent".into()),
        200,
        &json!({"body": "event: x\ndata: {}\n"}),
        true,
        Some("Hi! How can I help you?".into()),
        &test_call(),
        LEVEL,
    );
    let block = capture_record_to_agenticmd_block(&resp).unwrap();
    assert_eq!(block.body, "Hi! How can I help you?");
}

#[test]
fn skip_internal_suggestion_request_and_silent_response() {
    let req = llm_request_summary_record(
        Some("s".into()),
        None,
        "m",
        "/v1/messages",
        100,
        "messages",
        "anthropic",
        None,
        None,
        &test_call(),
        LEVEL,
        None,
    );
    assert!(skip_markdown_block(&req));
    let resp = llm_response_record_with_content(
        Some("s".into()),
        None,
        200,
        &json!({"body": "event: x\ndata: {}\n"}),
        true,
        Some(String::new()),
        &test_call(),
        LEVEL,
    );
    assert!(skip_markdown_block(&resp));
}

#[test]
fn skip_count_tokens_request() {
    let req = llm_request_summary_record(
        Some("s".into()),
        None,
        "m",
        "/v1/messages/count_tokens",
        1000,
        "count_tokens",
        "anthropic",
        Some("huge context".into()),
        None,
        &test_call(),
        LEVEL,
        None,
    );
    assert!(skip_markdown_block(&req));
}

#[test]
fn subagent_link_fields_in_markdown_block() {
    use crate::session::storage::CaptureRoute;
    use crate::subagent_link::enrich_record;

    let mut registry = crate::subagent_link::SubagentRegistry::default();
    let route = CaptureRoute {
        root_session: Some("run-1".into()),
        session_id: "sess".into(),
        storage_session_id: "agent-abc".into(),
        subagent_id: Some("abc".into()),
    };
    let mut rec = llm_response_record_with_content(
        Some("sess".into()),
        Some("proxy".into()),
        200,
        &json!({}),
        false,
        Some("done".into()),
        &test_call(),
        LEVEL,
    );
    let _outcome = enrich_record(
        &mut rec,
        &route,
        &axum::http::HeaderMap::new(),
        None,
        Some("done"),
        &mut registry,
    );
    let block = capture_record_to_agenticmd_block(&rec).unwrap();
    assert_eq!(
        block
            .header
            .fields
            .get("subagent_id")
            .and_then(|v| v.as_str()),
        Some("abc")
    );
    assert_eq!(
        block
            .header
            .fields
            .get("subagent_trajectory")
            .and_then(|v| v.as_str()),
        Some("agent-abc.md")
    );
    assert!(block.body.contains("persisting:subagent-self"));

    let imported = persisting_pchronicle::agenticmd_block_to_event_record(&block).unwrap();
    let visible = imported.visible_assistant_text().unwrap_or_default();
    assert_eq!(visible, "done");
    assert!(!visible.contains("persisting:subagent"));
}

#[test]
fn skip_main_flash_companion_user_duplicate() {
    let mut rec = llm_request_summary_record(
        Some("sess".into()),
        Some("proxy".into()),
        "deepseek-v4-flash",
        "/v1/messages",
        100,
        "messages",
        "anthropic",
        Some("再次开三个subagent".into()),
        None,
        &test_call(),
        LEVEL,
        None,
    );
    assert!(skip_markdown_block(&rec));
    rec.subagent_id = Some("abc".into());
    assert!(!skip_markdown_block(&rec));
}
