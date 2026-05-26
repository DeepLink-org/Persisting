//! Regression tests using agentgateway LLM JSON fixtures (copied under `tests/fixtures/`).

use bytes::Bytes;
use persisting_capture::conversion::{
    completions_response_to_messages, messages_request_to_completions,
    translate_completions_sse_to_messages, ProtocolBridge,
};
use persisting_capture::models_list::build_models_response;
use persisting_capture::protocol::ProtocolKind;
use persisting_capture::usage::extract_usage_from_response;
use persisting_capture::ProxyConfig;
use serde_json::Value;

const COMPLETIONS_BASIC_REQ: &str = include_str!("fixtures/requests/completions/basic.json");

#[test]
fn fixture_request_parses_model() {
    let v: Value = serde_json::from_str(COMPLETIONS_BASIC_REQ).unwrap();
    let model = v.get("model").and_then(|m| m.as_str()).unwrap();
    assert!(!model.is_empty());
}

#[test]
fn fixture_response_usage() {
    let body: Value =
        serde_json::from_str(include_str!("fixtures/response/completions/basic.json")).unwrap();
    let u = extract_usage_from_response(&body);
    assert!(u.total_tokens > 0 || u.input_tokens > 0 || u.output_tokens > 0);
}

#[test]
fn protocol_from_openai_paths() {
    assert_eq!(
        ProtocolKind::from_path("/v1/chat/completions"),
        ProtocolKind::ChatCompletions
    );
    assert_eq!(
        ProtocolKind::from_path("/v1/messages"),
        ProtocolKind::Messages
    );
}

#[test]
fn messages_to_completions_fixture() {
    let body = Bytes::from_static(include_bytes!("fixtures/requests/messages/basic.json"));
    let out = messages_request_to_completions(&body, "upstream-model").unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["model"], "upstream-model");
    assert_eq!(v["messages"][0]["content"], "Hello, world");
}

#[test]
fn completions_to_messages_fixture() {
    let body = Bytes::from_static(include_bytes!("fixtures/response/completions/basic.json"));
    let out = completions_response_to_messages(&body, "claude-test").unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["content"][0]["type"], "text");
}

#[test]
fn stream_translate_fixture() {
    let raw = include_str!("fixtures/local/response/completions/stream_head.txt");
    let out = translate_completions_sse_to_messages(raw, "claude-test").unwrap();
    assert!(out.contains("message_start"));
    assert!(out.contains("content_block_delta"));
}

#[test]
fn protocol_bridge_when_no_anthropic_upstream() {
    let cfg = ProxyConfig::from_toml_str(
        r#"
listen = "127.0.0.1:1"

[[models]]
name = "*"
upstream = "http://x/v1"
"#,
    )
    .unwrap();
    assert_eq!(
        ProtocolBridge::needed(ProtocolKind::Messages, &cfg.models[0]),
        ProtocolBridge::MessagesToCompletions
    );
}

#[test]
fn models_list_forward_target() {
    let cfg = ProxyConfig::from_toml_str(
        r#"
listen = "127.0.0.1:1"

[[models]]
name = "deepseek-chat"
upstream = "http://x/v1"

[[models]]
name = "*"
forward = "deepseek-chat"
"#,
    )
    .unwrap();
    let list = build_models_response(&cfg);
    let ids: Vec<_> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"deepseek-chat"));
}
