use persisting_pchronicle::model::StorylinePrompt;
use proptest::prelude::*;

fn text_strategy() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..96)
        .prop_map(|characters| characters.into_iter().collect())
}

proptest! {
    #![proptest_config(proptest::test_runner::Config::with_failure_persistence(
        proptest::test_runner::FileFailurePersistence::WithSource("regressions"),
    ))]

    #[test]
    fn public_prompt_pairs_are_present_exactly_when_nonempty(
        system in text_strategy(),
        user in text_strategy(),
    ) {
        let prompt = StorylinePrompt::from_pair(&system, &user);
        prop_assert_eq!(prompt.is_some(), !system.is_empty() || !user.is_empty());
        if let Some(prompt) = prompt {
            prop_assert_eq!(prompt.pair(), (system, user));
            prop_assert_eq!(prompt.has_nonempty_field(), !prompt.is_empty());
        }
    }
}
