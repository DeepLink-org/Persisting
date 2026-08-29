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
}
