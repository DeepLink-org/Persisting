//! Conversion + capture regression tests driven by agentgateway LLM fixtures.
//!
//! Source and license: `tests/fixtures/README.md`.

mod support;

use std::collections::BTreeMap;

use persisting_gateway::conversion::{
    completions_request_to_gemini, completions_response_to_messages,
    gemini_response_to_completions, messages_request_to_completions,
    responses_request_to_completions, translate_request_for_bridge, ProtocolBridge,
    StreamTranslator,
};
use persisting_gateway::dialogue_extract::{
    count_visible_user_messages, extract_assistant_text_from_json, extract_assistant_turn_from_sse,
    extract_user_message_from_request_body,
};
use persisting_gateway::protocol::ProtocolKind;
use persisting_gateway::understanding::understand_request;
use persisting_gateway::usage::{extract_usage_from_response, extract_usage_from_sse};
use serde_json::Value;
use support::ag_capture_cases::{
    ASSISTANT_JSON_CASES, ASSISTANT_SSE_CASES, USAGE_JSON_FIXTURES, USER_CAPTURE_CASES,
};
use support::ag_fixtures::{
    ag_snap_request, ag_snap_response, assert_json_eq, assert_messages_response_eq,
    client_model_from_completions_fixture, completions_messages_snap, fixture_exists,
    for_each_existing, for_each_existing_case, load_json_fixture, messages_completions_snap,
    parse_ag_sse_snap, parse_sse_events, read_fixture, read_fixture_bytes, sse_event_names,
    translate_openai_sse_fixture, upstream_model_from_messages_fixture, CaseReport,
    COMPLETIONS_TO_GEMINI, COMPLETIONS_TO_MESSAGES, GEMINI_TO_COMPLETIONS, MESSAGES_TO_COMPLETIONS,
    RESPONSES_TO_COMPLETIONS,
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
            let expected = ag_snap_request(&snap_path);
            assert_json_eq(&actual, &expected, case);
        },
    );
    report.assert_min_ran(3, "messages→completions snap");
}

#[test]
fn chronicle_semantic_ir_messages_request_matches_completions_snap() {
    let report = for_each_existing_case(
        MESSAGES_TO_COMPLETIONS,
        "requests/messages/",
        ".completions.snap",
        |case| {
            let snap_path = messages_completions_snap(case);
            let body = read_fixture_bytes(&format!("requests/messages/{case}.json"));
            let upstream = upstream_model_from_messages_fixture(case);
            let parsed =
                understand_request(persisting_gateway::protocol::ProtocolKind::Messages, &body)
                    .unwrap();
            let out = translate_request_for_bridge(
                ProtocolBridge::MessagesToCompletions,
                &parsed.semantic,
                &upstream,
                None,
            )
            .unwrap();
            let actual: Value = serde_json::from_slice(&out).unwrap();
            let expected = ag_snap_request(&snap_path);
            assert_json_eq(&actual, &expected, case);
        },
    );
    report.assert_min_ran(3, "Chronicle semantic IR messages→completions snap");
}

#[test]
fn ag_messages_tool_history_survives_without_tool_declarations() {
    let body = read_fixture_bytes("requests/messages/tool_history_without_tools.json");
    let out = messages_request_to_completions(&body, "upstream-model").unwrap();
    let value: Value = serde_json::from_slice(&out).unwrap();
    assert!(value.get("tools").is_none());
    assert_eq!(value["messages"][0]["role"], "assistant");
    assert_eq!(value["messages"][0]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        value["messages"][0]["tool_calls"][0]["function"]["name"],
        "get_weather"
    );
    assert_eq!(value["messages"][1]["role"], "tool");
    assert_eq!(value["messages"][1]["tool_call_id"], "call_1");
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

#[test]
fn chronicle_semantic_ir_responses_bridge_is_usable() {
    let report = for_each_existing_case(
        RESPONSES_TO_COMPLETIONS,
        "requests/responses/",
        ".json",
        |case| {
            let body = read_fixture_bytes(&format!("requests/responses/{case}.json"));
            let parsed =
                understand_request(persisting_gateway::protocol::ProtocolKind::Responses, &body)
                    .unwrap();
            let semantic = translate_request_for_bridge(
                ProtocolBridge::ResponsesToCompletions,
                &parsed.semantic,
                "upstream-model",
                None,
            )
            .unwrap();
            let semantic: Value = serde_json::from_slice(&semantic).unwrap();
            assert_eq!(semantic["model"], "upstream-model", "{case}");
            assert!(
                semantic["messages"]
                    .as_array()
                    .is_some_and(|messages| !messages.is_empty()),
                "{case}: semantic renderer returned no messages"
            );
        },
    );
    report.assert_min_ran(5, "Chronicle semantic IR responses→completions");
}

// --- conversion: chat completions ↔ Gemini native ---

#[test]
fn ag_completions_request_matches_gemini_native_goldens() {
    let report = for_each_existing_case(
        COMPLETIONS_TO_GEMINI,
        "requests/completions/",
        ".vertex-gemini.snap",
        |case| {
            let body = read_fixture_bytes(&format!("requests/completions/{case}.json"));
            let request: Value = serde_json::from_slice(&body).unwrap();
            let model = request
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("gemini-2.5-pro");
            let out = completions_request_to_gemini(&body, model).unwrap();
            let actual: Value = serde_json::from_slice(&out).unwrap();
            let expected =
                ag_snap_request(&format!("requests/completions/{case}.vertex-gemini.snap"));
            assert_json_eq(&actual, &expected, case);

            let parsed = understand_request(
                persisting_gateway::protocol::ProtocolKind::ChatCompletions,
                &body,
            )
            .unwrap();
            let typed = translate_request_for_bridge(
                ProtocolBridge::CompletionsToGemini,
                &parsed.semantic,
                model,
                None,
            )
            .unwrap();
            let typed: Value = serde_json::from_slice(&typed).unwrap();
            assert_json_eq(&typed, &expected, &format!("typed:{case}"));
        },
    );
    report.assert_min_ran(10, "completions→Gemini native snap");
}

#[test]
fn ag_gemini_native_response_matches_completions_goldens() {
    let report = for_each_existing_case(
        GEMINI_TO_COMPLETIONS,
        "response/vertex-gemini/",
        ".vertex-gemini-completions.snap",
        |case| {
            let body = read_fixture_bytes(&format!("response/vertex-gemini/{case}.json"));
            let out = gemini_response_to_completions(&body, "gemini-2.5-pro").unwrap();
            let mut actual: Value = serde_json::from_slice(&out).unwrap();
            actual["id"] = serde_json::json!("[id]");
            actual["created"] = serde_json::json!("[date]");
            let expected = ag_snap_response(&format!(
                "response/vertex-gemini/{case}.vertex-gemini-completions.snap"
            ));
            assert_json_eq(&actual, &expected, case);
        },
    );
    report.assert_min_ran(4, "Gemini native→completions snap");
}

#[test]
fn ag_gemini_native_stream_matches_completions_golden() {
    let input = read_fixture("response/vertex-gemini/stream_tool.json");
    let mut translator = StreamTranslator::new(
        ProtocolBridge::CompletionsToGemini,
        ProtocolKind::ChatCompletions,
        "gemini-2.5-pro",
    )
    .unwrap();
    let mut actual = Vec::new();
    for chunk in input.as_bytes().chunks(37) {
        actual.extend_from_slice(&translator.push_chunk(chunk).unwrap());
    }
    actual.extend_from_slice(&translator.finish_stream().unwrap());
    let actual = String::from_utf8(actual).unwrap();

    let expected = parse_ag_sse_snap(
        "response/vertex-gemini/stream_tool.vertex-gemini-completions-streaming.snap",
    );
    assert_eq!(fold_chat_sse(&actual), fold_chat_sse(&expected));
    assert_eq!(translator.metrics().usage.input_tokens, 15);
    assert_eq!(translator.metrics().usage.output_tokens, 12);
}

#[test]
fn gemini_native_stream_composes_with_messages_and_responses_bridges() {
    let input = read_fixture("response/vertex-gemini/stream_tool.json");

    let mut messages = StreamTranslator::new(
        ProtocolBridge::MessagesToGemini,
        ProtocolKind::Messages,
        "claude-gemini-client",
    )
    .unwrap();
    let mut messages_sse = messages.push_chunk(input.as_bytes()).unwrap().to_vec();
    messages_sse.extend_from_slice(&messages.finish_stream().unwrap());
    let messages_sse = String::from_utf8(messages_sse).unwrap();
    let message_events = sse_event_names(&messages_sse);
    assert!(message_events.contains(&"message_start"));
    assert!(message_events.contains(&"content_block_start"));
    assert!(message_events.contains(&"message_stop"));

    let mut responses = StreamTranslator::new(
        ProtocolBridge::ResponsesToGemini,
        ProtocolKind::Responses,
        "responses-gemini-client",
    )
    .unwrap();
    let mut responses_sse = responses.push_chunk(input.as_bytes()).unwrap().to_vec();
    responses_sse.extend_from_slice(&responses.finish_stream().unwrap());
    let responses_sse = String::from_utf8(responses_sse).unwrap();
    let response_events = sse_event_names(&responses_sse);
    assert!(response_events.contains(&"response.created"));
    assert!(response_events.contains(&"response.output_item.added"));
    assert!(response_events.contains(&"response.completed"));
}

// --- streaming translation ---

#[test]
fn ag_completions_stream_translates_to_messages_sse() {
    let path = "response/completions/stream.json";
    if !fixture_exists(path) {
        panic!("missing required fixture {path}");
    }
    let client_model = "claude-test";
    let mut translator = StreamTranslator::new(
        ProtocolBridge::MessagesToCompletions,
        ProtocolKind::Messages,
        client_model,
    )
    .unwrap();
    let out = translate_openai_sse_fixture(path, |chunk| translator.push_chunk(chunk));
    let events = sse_event_names(&out);
    assert!(events.contains(&"message_start"), "events: {events:?}");
    assert!(events.contains(&"content_block_delta"));
    let text = translator.streaming_capture_snapshot().unwrap_or_default();
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
fn ag_completions_stream_matches_messages_golden() {
    for (case, model) in [
        ("stream", "gpt-5-nano-2025-08-07"),
        ("stream_tool_empty_content", "claude-opus-4-8"),
    ] {
        let input = format!("response/completions/{case}.json");
        let snapshot = format!("response/completions/{case}.completions-messages-streaming.snap");
        let mut translator = StreamTranslator::new(
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
            model,
        )
        .unwrap();
        let mut actual = translate_openai_sse_fixture(&input, |chunk| translator.push_chunk(chunk));
        actual.push_str(std::str::from_utf8(&translator.finish_stream().unwrap()).unwrap());
        let expected = parse_ag_sse_snap(&snapshot);
        assert_eq!(
            normalized_message_events(&actual),
            normalized_message_events(&expected),
            "{case}"
        );
    }
}

#[test]
fn ag_local_stream_head_fixture() {
    let raw = read_fixture("local/response/completions/stream_head.txt");
    let out = completions_sse_to_messages(&raw, "claude-test");
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
            completions_sse_to_messages(&raw, "claude-test")
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
    let text = extract_assistant_turn_from_sse(&completions_sse_to_messages(&raw, "claude-test"));
    assert!(text.contains("Hi"));
}

fn completions_sse_to_messages(raw: &str, model: &str) -> String {
    let mut translator = StreamTranslator::new(
        ProtocolBridge::MessagesToCompletions,
        ProtocolKind::Messages,
        model,
    )
    .unwrap();
    let mut output = Vec::new();
    for chunk in raw.as_bytes().chunks(512) {
        output.extend_from_slice(&translator.push_chunk(chunk).unwrap());
    }
    output.extend_from_slice(&translator.finish_stream().unwrap());
    String::from_utf8(output).unwrap()
}

fn fold_chat_sse(sse: &str) -> Value {
    let mut id = None;
    let mut model = None;
    let mut role = None;
    let mut content = String::new();
    let mut finish_reason = None;
    let mut tools: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
    let mut usage = None;
    for line in sse.lines().filter_map(|line| line.strip_prefix("data: ")) {
        if line == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(line).unwrap();
        id = value
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(id);
        model = value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(model);
        if let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            role = delta
                .get("role")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(role);
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                content.push_str(text);
            }
            for call in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                let tool = tools.entry(index).or_default();
                if let Some(call_id) = call.get("id").and_then(Value::as_str) {
                    tool.0 = call_id.to_string();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    tool.1 = name.to_string();
                }
                if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                {
                    tool.2.push_str(arguments);
                }
            }
            finish_reason = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(finish_reason);
        }
        if let Some(value_usage) = value.get("usage") {
            usage = Some(serde_json::json!({
                "prompt_tokens": value_usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
                "completion_tokens": value_usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
                "total_tokens": value_usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
            }));
        }
    }
    serde_json::json!({
        "id": id,
        "model": model,
        "role": role,
        "content": content,
        "finish_reason": finish_reason,
        "tool_calls": tools.into_values().map(|(id, name, arguments)| serde_json::json!({"id":id,"name":name,"arguments":arguments})).collect::<Vec<_>>(),
        "usage": usage,
    })
}

fn normalized_message_events(sse: &str) -> Vec<(String, Value)> {
    let mut events = parse_sse_events(sse);
    for (_, value) in &mut events {
        let Some(usage) = value.get_mut("usage").and_then(Value::as_object_mut) else {
            continue;
        };
        for key in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
            if usage.get(key).and_then(Value::as_u64) == Some(0) {
                usage.remove(key);
            }
        }
    }
    events
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
    assert!(user.contains("[image: base64:"));
    assert!(user.contains("image/png"));
}

#[test]
fn ag_messages_full_converted_user_still_has_image_placeholder() {
    let path = "requests/completions/full.json";
    if !fixture_exists(path) {
        return;
    }
    let body = read_fixture_bytes(path);
    let user = extract_user_message_from_request_body(&body).unwrap();
    assert!(user.contains("[image: base64:"));
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
