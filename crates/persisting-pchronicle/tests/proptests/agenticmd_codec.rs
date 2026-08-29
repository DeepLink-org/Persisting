use persisting_pchronicle::document::{decode_agenticmd, encode_agenticmd};
use persisting_pchronicle::model::StorylineDocument;
use proptest::prelude::*;
use serde_json::json;

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_agenticmd_codec_roundtrips_generated_turns(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        messages in proptest::collection::vec("[A-Za-z0-9 .,!?_-]{0,96}", 0..10),
    ) {
        let mut story = StorylineDocument::new(session_id, agent_id);
        story.turns = messages.into_iter().enumerate().map(|(index, message)| {
            serde_json::from_value(json!({
                "id": index as i64,
                "src": if index % 2 == 0 { "user" } else { "agent" },
                "msg": message,
            })).unwrap()
        }).collect();
        let encoded = encode_agenticmd(&story).unwrap();
        let decoded = decode_agenticmd(&encoded).unwrap();
        prop_assert_eq!(decoded, story);
    }

    #[test]
    fn public_agenticmd_codec_preserves_positive_turn_ids(
        session_id in "[A-Za-z0-9._-]{1,24}",
        agent_id in "[A-Za-z0-9._-]{1,24}",
        messages in proptest::collection::vec("[A-Za-z0-9 .,!?_-]{0,96}", 1..8),
    ) {
        let mut story = StorylineDocument::new(session_id, agent_id);
        story.turns = messages.into_iter().enumerate().map(|(index, message)| {
            serde_json::from_value(json!({
                "id": (index + 1) as i64,
                "src": if index % 2 == 0 { "user" } else { "agent" },
                "msg": message,
            })).unwrap()
        }).collect();
        let decoded = decode_agenticmd(&encode_agenticmd(&story).unwrap()).unwrap();
        prop_assert_eq!(
            decoded.turns.iter().map(|turn| turn.id).collect::<Vec<_>>(),
            story.turns.iter().map(|turn| turn.id).collect::<Vec<_>>(),
        );
    }
}
