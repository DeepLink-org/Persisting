use persisting_pchronicle::document::{decode_agenticmd, encode_agenticmd};
use persisting_pchronicle::model::StorylineDocument;
use proptest::prelude::*;

fn token_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9._-]{1,32}").unwrap()
}

fn text_strategy() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap()
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_agenticmd_roundtrip_preserves_foreign_unknown_fields(
        key in token_strategy(),
        value in text_strategy(),
    ) {
        let mut story = StorylineDocument::new("session", "agent");
        let pointer = format!("/vendor/{key}");
        story
            .unknown_fields
            .insert(
                "future-format",
                "source",
                &pointer,
                serde_json::json!(value),
            )
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();

        let encoded = encode_agenticmd(&story).unwrap();
        let decoded = decode_agenticmd(&encoded).unwrap();
        prop_assert_eq!(decoded.unknown_fields, story.unknown_fields);
        prop_assert_eq!(decoded.unknown_key_counts, story.unknown_key_counts);
    }

    #[test]
    fn public_agenticmd_text_messages_roundtrip_without_json_reencoding(
        message in text_strategy(),
    ) {
        let mut story = StorylineDocument::new("session", "agent");
        story.turns.push(persisting_pchronicle::model::StorylineTurn {
            id: 1,
            kind: Some("message".into()),
            timestamp: None,
            source: "user".into(),
            message: serde_json::Value::String(message.clone()),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        });

        let encoded = encode_agenticmd(&story).unwrap();
        prop_assert!(!encoded.contains("message_encoding: json"));
        let decoded = decode_agenticmd(&encoded).unwrap();
        prop_assert_eq!(&decoded.turns[0].message, &serde_json::Value::String(message));
    }

    #[test]
    fn public_agenticmd_roundtrip_preserves_turn_timing_and_model_metadata(
        message in text_strategy(),
        latency in 0i64..60_000,
        ttft in 0i64..60_000,
        model in token_strategy(),
    ) {
        let mut story = StorylineDocument::new("session", "agent");
        story.turns.push(persisting_pchronicle::model::StorylineTurn {
            id: 1,
            kind: Some("message".into()),
            timestamp: Some(
                persisting_pchronicle::model::StorylineTimestamp::from_rfc3339(
                    "2026-08-17T01:02:03.123456789Z",
                ).unwrap(),
            ),
            source: "agent".into(),
            message: serde_json::Value::String(message),
            reasoning_content: None,
            reasoning_effort: Some(serde_json::json!("high")),
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: Some(model),
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms: Some(latency),
            ttft_ms: Some(ttft),
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        });
        let decoded = decode_agenticmd(&encode_agenticmd(&story).unwrap()).unwrap();
        prop_assert_eq!(&decoded.turns, &story.turns);
    }
}
