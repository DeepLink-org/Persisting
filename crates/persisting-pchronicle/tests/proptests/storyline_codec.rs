use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines,
};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use proptest::prelude::*;
use serde_json::json;

fn text_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..96)
        .prop_map(|characters| characters.into_iter().collect())
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_storyline_codec_roundtrips_generated_turn_sequences(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        messages in proptest::collection::vec(text_strategy(), 0..12),
    ) {
        let mut story = StorylineDocument::new(session_id, agent_id);
        story.turns = messages.into_iter().enumerate().map(|(index, message)| {
            serde_json::from_value::<StorylineTurn>(json!({
                "id": index as i64,
                "src": if index % 2 == 0 { "user" } else { "agent" },
                "msg": message,
            })).unwrap()
        }).collect();
        let encoded = encode_json_storylines(DocumentFormat::Storyline, &[story.clone()]).unwrap();
        let decoded = decode_json_storylines(
            DocumentFormat::Storyline,
            &encoded.to_string(),
            "generated.storyline.json",
        ).unwrap();
        prop_assert_eq!(decoded, vec![story]);
    }

    #[test]
    fn public_storyline_wire_uses_only_short_canonical_field_names(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        message in text_strategy(),
    ) {
        let mut story = StorylineDocument::new(session_id, agent_id);
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
            timestamp: None,
            source: "user".into(),
            message: json!(message),
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
        let encoded = encode_json_storylines(DocumentFormat::Storyline, &[story]).unwrap();
        let root = encoded.as_object().unwrap();
        for legacy in ["session_id", "agent_id", "source", "message"] {
            prop_assert!(!root.contains_key(legacy), "legacy root key {legacy}");
        }
        let turn = &root["turns"][0];
        prop_assert!(turn.get("src").is_some());
        prop_assert!(turn.get("msg").is_some());
        prop_assert!(turn.get("source").is_none());
        prop_assert!(turn.get("message").is_none());
    }
}
