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
}
