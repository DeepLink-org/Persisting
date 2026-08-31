use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines,
};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use proptest::prelude::*;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_atif_codec_preserves_generated_story_identity(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        trajectory_id in prop::option::of("[A-Za-z0-9._-]{1,24}"),
    ) {
        let mut story = StorylineDocument::new(session_id.clone(), agent_id.clone());
        story.trajectory_id = trajectory_id.clone();
        let encoded = encode_json_storylines(DocumentFormat::Atif, &[story]).unwrap();
        prop_assert_eq!(encoded["schema_version"].as_str(), Some("ATIF-v1.7"));
        prop_assert!(!encoded["agent"]["version"].as_str().unwrap_or_default().is_empty());
        let decoded = decode_json_storylines(
            DocumentFormat::Atif,
            &encoded.to_string(),
            "generated.atif.json",
        ).unwrap();
        prop_assert_eq!(decoded.len(), 1);
        prop_assert_eq!(&decoded[0].session_id, &session_id);
        prop_assert_eq!(&decoded[0].agent.id, &agent_id);
        prop_assert_eq!(&decoded[0].trajectory_id, &trajectory_id);
    }

    #[test]
    fn public_atif_codec_preserves_generated_turn_content(
        session_id in "[A-Za-z0-9._-]{1,24}",
        message in "[A-Za-z0-9 .,!?_:/-]{0,96}",
    ) {
        let mut story = StorylineDocument::new(session_id, "agent");
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
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
        let encoded = encode_json_storylines(DocumentFormat::Atif, &[story]).unwrap();
        let decoded = decode_json_storylines(DocumentFormat::Atif, &encoded.to_string(), "generated.atif.json").unwrap();
        prop_assert_eq!(decoded.len(), 1);
        prop_assert_eq!(decoded[0].turns.len(), 1);
        prop_assert_eq!(&decoded[0].turns[0].source, "user");
        prop_assert_eq!(&decoded[0].turns[0].message, &serde_json::Value::String(message));
    }
}
