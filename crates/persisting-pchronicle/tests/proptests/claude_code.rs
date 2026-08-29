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
}
