use persisting_pchronicle::document::{DocumentFormat, decode_json_storylines};
use proptest::prelude::*;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9._-]{1,24}").unwrap()
}

fn text_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-zA-Z0-9 _.,:/-]{1,96}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_claude_user_decode_preserves_session_and_text(
        session_id in token_strategy(),
        text in text_strategy(),
    ) {
        let event = serde_json::json!({
            "type": "user",
            "sessionId": session_id,
            "uuid": "event-1",
            "message": {"role": "user", "content": text.clone()}
        });
        let stories = decode_json_storylines(
            DocumentFormat::ClaudeCode,
            &format!("{event}\n"),
            "session.jsonl",
        )
        .unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].session_id.as_str(), session_id.as_str());
        prop_assert_eq!(stories[0].turns.len(), 1);
        prop_assert_eq!(&stories[0].turns[0].message, &serde_json::Value::String(text));
        prop_assert_eq!(stories[0].turns[0].source.as_str(), "user");
    }

    #[test]
    fn public_claude_assistant_decode_preserves_model_and_text(
        session_id in token_strategy(),
        model in token_strategy(),
        text in text_strategy(),
    ) {
        let event = serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "uuid": "event-1",
            "message": {"model": model.clone(), "content": [{"type": "text", "text": text.clone()}]}
        });
        let stories = decode_json_storylines(
            DocumentFormat::ClaudeCode,
            &format!("{event}\n"),
            "session.jsonl",
        )
        .unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 1);
        prop_assert_eq!(&stories[0].turns[0].source, "agent");
        prop_assert_eq!(&stories[0].turns[0].message, &serde_json::Value::String(text));
        prop_assert_eq!(stories[0].agent.model_name.as_deref(), Some(model.as_str()));
    }

    #[test]
    fn public_claude_tool_results_attach_to_the_originating_call(
        session_id in token_strategy(),
        tool_id in token_strategy(),
        tool_name in token_strategy(),
        result in text_strategy(),
    ) {
        let assistant = serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "uuid": "assistant-1",
            "message": {
                "model": "model",
                "content": [{
                    "type": "tool_use",
                    "id": tool_id.clone(),
                    "name": tool_name.clone(),
                    "input": {"arg": result.clone()}
                }]
            }
        });
        let user = serde_json::json!({
            "type": "user",
            "sessionId": session_id,
            "uuid": "user-1",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_id.clone(),
                    "content": result.clone()
                }]
            }
        });
        let input = format!("{assistant}\n{user}\n");
        let stories = decode_json_storylines(DocumentFormat::ClaudeCode, &input, "session.jsonl")
            .unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 1);
        let calls = stories[0].turns[0].tool_calls.as_ref().unwrap();
        prop_assert_eq!(calls.len(), 1);
        prop_assert_eq!(&calls[0].tool_call_id, &tool_id);
        prop_assert_eq!(&calls[0].function_name, &tool_name);
        prop_assert_eq!(&calls[0].result, &Some(serde_json::Value::String(result)));
    }

    #[test]
    fn public_claude_thinking_content_is_preserved_as_reasoning(
        session_id in token_strategy(),
        thinking in text_strategy(),
        text in text_strategy(),
    ) {
        let event = serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "uuid": "event-1",
            "message": {
                "model": "model",
                "content": [
                    {"type": "thinking", "text": thinking.clone()},
                    {"type": "text", "text": text.clone()}
                ]
            }
        });
        let stories = decode_json_storylines(
            DocumentFormat::ClaudeCode,
            &format!("{event}\n"),
            "session.jsonl",
        ).unwrap();
        prop_assert_eq!(stories.len(), 1);
        prop_assert_eq!(stories[0].turns.len(), 1);
        prop_assert_eq!(stories[0].turns[0].reasoning_content.as_deref(), Some(thinking.as_str()));
        prop_assert_eq!(&stories[0].turns[0].message, &serde_json::Value::String(text));
    }
}
