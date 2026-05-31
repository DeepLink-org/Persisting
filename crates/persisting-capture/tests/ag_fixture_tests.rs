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
    count_visible_user_messages, extract_assistant_text_from_json, extract_assistant_turn_from_sse,
    extract_user_message_from_request_body,
};
use persisting_capture::usage::{extract_usage_from_response, extract_usage_from_sse};
use serde_json::Value;
use support::ag_capture_cases::{
    ASSISTANT_JSON_CASES, ASSISTANT_SSE_CASES, USAGE_JSON_FIXTURES, USER_CAPTURE_CASES,
};
use support::ag_fixtures::{
    ag_snap_response, assert_json_eq, assert_messages_response_eq,
    client_model_from_completions_fixture, completions_messages_snap, fixture_exists,
    for_each_existing, for_each_existing_case, load_json_fixture, messages_completions_snap,
    parse_ag_sse_snap, read_fixture, read_fixture_bytes, sse_event_names,
    translate_openai_sse_fixture, upstream_model_from_messages_fixture, CaseReport,
    COMPLETIONS_TO_MESSAGES, MESSAGES_TO_COMPLETIONS, RESPONSES_TO_COMPLETIONS,
};

// --- conversion: messages ↔ completions ---

#[test]
fn ag_messages_request_matches_completions_snap() {
    let report = for_each_existing_case(
        MESSAGES_TO_COMPLETIONS,
        "requests/messages/",
        ".completions.snap",
        |case| {
            let snap_path = messages_completions_snap(case);
            let body = read_fixture_bytes(&format!("requests/messages/{case}.json"));
            let upstream = upstream_model_from_messages_fixture(case);
            let out = messages_request_to_completions(&body, &upstream).unwrap();
            let actual: Value = serde_json::from_slice(&out).unwrap();
            let expected = support::ag_fixtures::parse_ag_json_snap(&snap_path);
            assert_json_eq(&actual, &expected, case);
        },
    );
    report.assert_min_ran(3, "messages→completions snap");
}

#[test]
fn ag_completions_response_matches_messages_snap() {
    let report = for_each_existing_case(
        COMPLETIONS_TO_MESSAGES,
        "response/completions/",
        ".completions-messages.snap",
        |case| {
            let snap_path = completions_messages_snap(case);
            let body = read_fixture_bytes(&format!("response/completions/{case}.json"));
            let client_model = client_model_from_completions_fixture(case);
            let out = completions_response_to_messages(&body, &client_model).unwrap();
            let actual: Value = serde_json::from_slice(&out).unwrap();
            let expected = ag_snap_response(&snap_path);
            assert_messages_response_eq(&actual, &expected, case);
        },
    );
    report.assert_min_ran(5, "completions→messages snap");
}

// --- conversion: responses → completions ---

#[test]
fn ag_responses_request_to_completions() {
    let report = for_each_existing_case(
        RESPONSES_TO_COMPLETIONS,
        "requests/responses/",
        ".json",
        |case| {
            let body = read_fixture_bytes(&format!("requests/responses/{case}.json"));
            let out = responses_request_to_completions(&body, "upstream-model", None).unwrap();
            let v: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(v["model"], "upstream-model", "{case}");
            assert!(
                v.get("messages")
                    .and_then(|m| m.as_array())
                    .is_some_and(|a| !a.is_empty()),
                "{case}: expected non-empty messages"
            );
        },
    );
    report.assert_min_ran(5, "responses→completions");
}

// --- streaming translation ---

#[test]
fn ag_completions_stream_translates_to_messages_sse() {
    let path = "response/completions/stream.json";
    if !fixture_exists(path) {
        panic!("missing required fixture {path}");
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
        panic!("missing required fixture {snap_path}");
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

// --- capture: user dialogue matrix ---

#[test]
fn ag_capture_user_dialogue_matrix() {
    let mut report = CaseReport::default();
    for case in USER_CAPTURE_CASES {
        if !fixture_exists(case.path) {
            report.record_skipped();
            continue;
        }
        let body = read_fixture_bytes(case.path);
        let v = load_json_fixture(case.path);
        let turns = count_visible_user_messages(&v);
        assert!(
            turns >= case.min_turns,
            "{}: expected >={} user turns, got {turns}",
            case.path,
            case.min_turns
        );
        let user = extract_user_message_from_request_body(&body)
            .unwrap_or_else(|| panic!("{}: expected user content", case.path));
        assert!(!user.trim().is_empty(), "{}: empty user content", case.path);
        for needle in case.must_contain {
            assert!(
                user.contains(needle),
                "{}: user content missing {needle:?}\n---\n{user}",
                case.path
            );
        }
        report.record_ran();
    }
    report.assert_min_ran(8, "user dialogue matrix");
}

#[test]
fn ag_capture_user_from_messages_requests() {
    let report = for_each_existing_case(
        MESSAGES_TO_COMPLETIONS,
        "requests/messages/",
        ".json",
        |case| {
            let body = read_fixture_bytes(&format!("requests/messages/{case}.json"));
            let user = extract_user_message_from_request_body(&body);
            assert!(user.is_some(), "{case}: expected user text");
            assert!(!user.unwrap().trim().is_empty(), "{case}: empty user text");
        },
    );
    report.assert_min_ran(3, "messages request user extract");
}

#[test]
fn ag_capture_user_from_responses_requests() {
    let report = for_each_existing_case(
        RESPONSES_TO_COMPLETIONS,
        "requests/responses/",
        ".json",
        |case| {
            let body = read_fixture_bytes(&format!("requests/responses/{case}.json"));
            let user = extract_user_message_from_request_body(&body);
            assert!(user.is_some(), "{case}: expected user text");
        },
    );
    report.assert_min_ran(5, "responses request user extract");
}

// --- capture: assistant dialogue matrix ---

#[test]
fn ag_capture_assistant_json_matrix() {
    let mut report = CaseReport::default();
    for case in ASSISTANT_JSON_CASES {
        if !fixture_exists(case.path) {
            report.record_skipped();
            continue;
        }
        let body = load_json_fixture(case.path);
        let text = extract_assistant_text_from_json(&body)
            .unwrap_or_else(|| panic!("{}: expected assistant text", case.path));
        for needle in case.must_contain {
            assert!(
                text.contains(needle),
                "{}: assistant missing {needle:?}\n---\n{text}",
                case.path
            );
        }
        report.record_ran();
    }
    report.assert_min_ran(5, "assistant JSON matrix");
}

#[test]
fn ag_capture_assistant_sse_matrix() {
    let mut report = CaseReport::default();
    for case in ASSISTANT_SSE_CASES {
        if !fixture_exists(case.path) {
            report.record_skipped();
            continue;
        }
        let raw = read_fixture(case.path);
        let sse = if case.translate_completions_to_messages {
            translate_completions_sse_to_messages(&raw, "claude-test").unwrap()
        } else {
            raw
        };
        let text = extract_assistant_turn_from_sse(&sse);
        for needle in case.must_contain {
            assert!(
                text.contains(needle),
                "{}: SSE assistant missing {needle:?}\n---\n{text}",
                case.path
            );
        }
        report.record_ran();
    }
    report.assert_min_ran(4, "assistant SSE matrix");
}

#[test]
fn ag_capture_assistant_from_completions_response() {
    let body: Value = load_json_fixture("response/completions/basic.json");
    let text = extract_assistant_text_from_json(&body).unwrap();
    assert!(text.contains("Sorry") || text.contains("provider"));
}

#[test]
fn ag_capture_assistant_from_stream_fixture() {
    let raw = read_fixture("local/response/completions/stream_head.txt");
    let text = extract_assistant_turn_from_sse(
        &translate_completions_sse_to_messages(&raw, "claude-test").unwrap(),
    );
    assert!(text.contains("Hi"));
}

// --- capture: usage ---

#[test]
fn ag_capture_usage_json_matrix() {
    let report = for_each_existing(USAGE_JSON_FIXTURES, |path| {
        let body = load_json_fixture(path);
        let usage = extract_usage_from_response(&body);
        assert!(
            usage.total_tokens > 0 || usage.input_tokens + usage.output_tokens > 0,
            "{path}: expected non-zero usage, got {usage:?}"
        );
    });
    report.assert_min_ran(5, "usage JSON matrix");
}

#[test]
fn ag_capture_usage_from_completions_stream() {
    let raw = read_fixture("local/response/completions/stream_tool_call.txt");
    let usage = extract_usage_from_sse(&raw);
    assert!(usage.total_tokens > 0, "usage: {usage:?}");
}

#[test]
fn ag_capture_usage_from_completions_response() {
    let body = load_json_fixture("response/completions/basic.json");
    let usage = extract_usage_from_response(&body);
    assert!(usage.total_tokens > 0 || usage.input_tokens + usage.output_tokens > 0);
}

// --- capture: multimodal (Phase 0) ---

#[test]
fn ag_completions_full_fixture_includes_image_placeholder() {
    let path = "requests/completions/full.json";
    if !fixture_exists(path) {
        panic!("missing required fixture {path}");
    }
    let body = read_fixture_bytes(path);
    let user = extract_user_message_from_request_body(&body).unwrap();
    assert!(user.contains("What's in this image?"));
    assert!(user.contains("[image: url:"));
    assert!(user.contains("PNG_transparency_demonstration"));
}

#[test]
fn ag_messages_full_converted_user_still_has_image_placeholder() {
    let path = "requests/completions/full.json";
    if !fixture_exists(path) {
        return;
    }
    let body = read_fixture_bytes(path);
    let user = extract_user_message_from_request_body(&body).unwrap();
    assert!(user.contains("[image: url:"));
}

#[test]
fn ag_responses_stream_image_fixture_has_generation_events() {
    let path = "response/responses/stream-image.json";
    if !fixture_exists(path) {
        panic!("missing required fixture {path}");
    }
    let raw = read_fixture(path);
    assert!(raw.contains("image_generation_call"));
    let text = extract_assistant_turn_from_sse(&raw);
    assert!(text.contains("[image_generated:"));
}
