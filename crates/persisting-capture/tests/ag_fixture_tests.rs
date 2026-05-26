//! Conversion + capture regression tests driven by agentgateway LLM fixtures.
//!
//! Source and license: `tests/fixtures/README.md`.

mod support;

use persisting_capture::conversion::{
    completions_response_to_messages, messages_request_to_completions,
    responses_request_to_completions, translate_completions_sse_to_messages,
    CompletionsStreamTranslator,
};
use persisting_capture::dialogue_extract::{
    extract_assistant_text_from_json, extract_assistant_turn_from_sse,
    extract_user_message_from_request_body,
};
use persisting_capture::usage::extract_usage_from_response;
use serde_json::Value;
use support::ag_fixtures::{
    ag_snap_response, assert_json_eq, assert_messages_response_eq, client_model_from_completions_fixture,
    completions_messages_snap, fixture_exists, messages_completions_snap,
    parse_ag_sse_snap, read_fixture, read_fixture_bytes, sse_event_names,
    translate_openai_sse_fixture, upstream_model_from_messages_fixture, COMPLETIONS_TO_MESSAGES,
    MESSAGES_TO_COMPLETIONS, RESPONSES_TO_COMPLETIONS,
};

// --- messages → completions (request translation) ---

#[test]
fn ag_messages_request_matches_completions_snap() {
    for case in MESSAGES_TO_COMPLETIONS {
        let snap_path = messages_completions_snap(case);
        if !fixture_exists(&snap_path) {
            continue;
        }
        let body = read_fixture_bytes(&format!("requests/messages/{case}.json"));
        let upstream = upstream_model_from_messages_fixture(case);
        let out = messages_request_to_completions(&body, &upstream).unwrap();
        let actual: Value = serde_json::from_slice(&out).unwrap();
        let expected = support::ag_fixtures::parse_ag_json_snap(&snap_path);
        assert_json_eq(&actual, &expected, case);
    }
}

// --- completions → messages (response translation) ---

#[test]
fn ag_completions_response_matches_messages_snap() {
    for case in COMPLETIONS_TO_MESSAGES {
        let snap_path = completions_messages_snap(case);
        if !fixture_exists(&snap_path) {
            continue;
        }
        let body = read_fixture_bytes(&format!("response/completions/{case}.json"));
        let client_model = client_model_from_completions_fixture(case);
        let out = completions_response_to_messages(&body, &client_model).unwrap();
        let actual: Value = serde_json::from_slice(&out).unwrap();
        let expected = ag_snap_response(&snap_path);
        assert_messages_response_eq(&actual, &expected, case);
    }
}

// --- responses → completions ---

#[test]
fn ag_responses_request_to_completions() {
    for case in RESPONSES_TO_COMPLETIONS {
        let path = format!("requests/responses/{case}.json");
        if !fixture_exists(&path) {
            continue;
        }
        let body = read_fixture_bytes(&path);
        let out = responses_request_to_completions(&body, "upstream-model", None).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "upstream-model");
        assert!(v.get("messages").and_then(|m| m.as_array()).is_some_and(|a| !a.is_empty()));
    }
}

// --- streaming ---

#[test]
fn ag_completions_stream_translates_to_messages_sse() {
    let path = "response/completions/stream.json";
    if !fixture_exists(path) {
        return;
    }
    let client_model = "claude-test";
    let mut translator = CompletionsStreamTranslator::new(client_model);
    let out = translate_openai_sse_fixture(path, |chunk| translator.push_chunk(chunk));
    let events = sse_event_names(&out);
    assert!(events.contains(&"message_start"), "events: {events:?}");
    assert!(events.contains(&"content_block_delta"));
    let text = translator.accumulated_assistant_text();
    assert!(text.contains("Hi"), "accumulated: {text}");
    assert!(text.contains("help"));
}

#[test]
fn ag_stream_snap_contains_expected_events() {
    let snap_path = "response/completions/stream.completions-messages-streaming.snap";
    if !fixture_exists(snap_path) {
        return;
    }
    let snap = parse_ag_sse_snap(snap_path);
    let events = sse_event_names(&snap);
    assert!(events.contains(&"message_start"));
    assert!(events.contains(&"content_block_delta"));
}

#[test]
fn ag_local_stream_head_fixture() {
    let raw = read_fixture("local/response/completions/stream_head.txt");
    let out = translate_completions_sse_to_messages(&raw, "claude-test").unwrap();
    assert!(out.contains("message_start"));
    assert!(out.contains("content_block_delta"));
}

// --- capture: dialogue extraction ---

#[test]
fn ag_capture_user_from_messages_requests() {
    for case in MESSAGES_TO_COMPLETIONS {
        let path = format!("requests/messages/{case}.json");
        if !fixture_exists(&path) {
            continue;
        }
        let body = read_fixture_bytes(&path);
        let user = extract_user_message_from_request_body(&body);
        assert!(user.is_some(), "{case}: expected user text");
        assert!(!user.unwrap().trim().is_empty(), "{case}: empty user text");
    }
}

#[test]
fn ag_capture_user_from_responses_requests() {
    for case in RESPONSES_TO_COMPLETIONS {
        let path = format!("requests/responses/{case}.json");
        if !fixture_exists(&path) {
            continue;
        }
        let body = read_fixture_bytes(&path);
        let user = extract_user_message_from_request_body(&body);
        assert!(user.is_some(), "{case}: expected user text");
    }
}

#[test]
fn ag_capture_assistant_from_completions_response() {
    let body: Value =
        serde_json::from_str(&read_fixture("response/completions/basic.json")).unwrap();
    let text = extract_assistant_text_from_json(&body).unwrap();
    assert!(text.contains("Sorry") || text.contains("provider"));
}

#[test]
fn ag_capture_assistant_from_stream_fixture() {
    let raw = read_fixture("local/response/completions/stream_head.txt");
    let text = extract_assistant_turn_from_sse(&translate_completions_sse_to_messages(
        &raw,
        "claude-test",
    )
    .unwrap());
    assert!(text.contains("Hi"));
}

#[test]
fn ag_capture_usage_from_completions_response() {
    let body: Value =
        serde_json::from_str(&read_fixture("response/completions/basic.json")).unwrap();
    let usage = extract_usage_from_response(&body);
    assert!(usage.total_tokens > 0 || usage.input_tokens + usage.output_tokens > 0);
}

#[test]
fn ag_capture_codex_responses_fixture() {
    let path = "local/requests/responses/codex_basic.json";
    if !fixture_exists(path) {
        return;
    }
    let body = read_fixture_bytes(path);
    let user = extract_user_message_from_request_body(&body).unwrap();
    assert!(user.contains("hi") || user.contains("permissions"));
}
