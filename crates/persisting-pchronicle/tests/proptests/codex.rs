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
}
