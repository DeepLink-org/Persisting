use persisting_pchronicle::document::{DocumentFormat, decode_json_storylines};
use proptest::prelude::*;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9._-]{1,24}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_empty_codex_input_uses_the_source_filename_as_session_id(stem in token_strategy()) {
        let path = format!("rollout-{stem}.jsonl");
        let stories = decode_json_storylines(DocumentFormat::Codex, "", &path).unwrap();
        let expected_session = format!("rollout-{stem}");
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].session_id.as_str(), expected_session.as_str());
        prop_assert!(stories[0].turns.is_empty());
    }

    #[test]
    fn public_whitespace_only_codex_inputs_use_the_source_stem(
        whitespace in proptest::collection::vec(
            prop::sample::select(vec![' ', '\n', '\r', '\t']),
            0..64,
        ),
        stem in token_strategy(),
    ) {
        let path = format!("rollout-{stem}.jsonl");
        let input = whitespace.into_iter().collect::<String>();
        let stories = decode_json_storylines(DocumentFormat::Codex, &input, &path).unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(&stories[0].session_id, &format!("rollout-{stem}"));
        prop_assert!(stories[0].turns.is_empty());
    }

    #[test]
    fn public_codex_message_items_preserve_user_and_assistant_text(
        session_id in token_strategy(),
        user_text in proptest::string::string_regex("[A-Za-z0-9 _.,:/-]{1,96}").unwrap(),
        assistant_text in proptest::string::string_regex("[A-Za-z0-9 _.,:/-]{1,96}").unwrap(),
    ) {
        let lines = vec![
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:12.000Z",
                "type": "session_meta",
                "payload": {"id": session_id}
            }),
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:13.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": user_text}]
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:14.000Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": assistant_text}]
                }
            }),
        ];
        let input = lines
            .iter()
            .map(serde_json::Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let stories = decode_json_storylines(DocumentFormat::Codex, &input, "rollout.jsonl").unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(&stories[0].session_id, &session_id);
        prop_assert_eq!(stories[0].turns.len(), 2);
        prop_assert_eq!(&stories[0].turns[0].source, "user");
        prop_assert_eq!(&stories[0].turns[0].message, &serde_json::Value::String(user_text));
        prop_assert_eq!(&stories[0].turns[1].source, "agent");
        prop_assert_eq!(&stories[0].turns[1].message, &serde_json::Value::String(assistant_text));
    }

    #[test]
    fn public_codex_function_call_outputs_attach_to_their_call(
        session_id in token_strategy(),
        call_id in token_strategy(),
        function_name in token_strategy(),
        output in proptest::string::string_regex("[A-Za-z0-9 _.,:/-]{1,96}").unwrap(),
    ) {
        let lines = vec![
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:12.000Z",
                "type": "session_meta",
                "payload": {"id": session_id}
            }),
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:13.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": function_name.clone(),
                    "arguments": "{\"arg\":\"value\"}",
                    "call_id": call_id.clone()
                }
            }),
            serde_json::json!({
                "timestamp": "2026-08-03T08:15:14.000Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": call_id.clone(),
                    "output": output.clone()
                }
            }),
        ];
        let input = lines.iter().map(serde_json::Value::to_string).collect::<Vec<_>>().join("\n");
        let stories = decode_json_storylines(DocumentFormat::Codex, &input, "rollout.jsonl").unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 1);
        let calls = stories[0].turns[0].tool_calls.as_ref().unwrap();
        prop_assert_eq!(calls.len(), 1);
        prop_assert_eq!(&calls[0].tool_call_id, &call_id);
        prop_assert_eq!(&calls[0].function_name, &function_name);
        prop_assert_eq!(&calls[0].result, &Some(serde_json::Value::String(output)));
    }
}
