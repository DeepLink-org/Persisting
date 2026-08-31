use persisting_pchronicle::model::{StorylinePrompt, StorylineToolCall, StorylineTurn};
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

    #[test]
    fn public_turn_effective_kind_respects_explicit_kind_and_tool_presence(
        source in prop::sample::select(vec!["user", "agent", "system", "other"]),
        explicit_kind in prop::option::of("[A-Za-z0-9_-]{1,16}"),
        has_tool_call in any::<bool>(),
    ) {
        let mut turn = StorylineTurn {
            id: 1,
            kind: explicit_kind.clone(),
            timestamp: None,
            source: source.into(),
            message: json!("message"),
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
        };
        if has_tool_call {
            turn.tool_calls = Some(vec![StorylineToolCall {
                tool_call_id: "call".into(),
                function_name: "tool".into(),
                arguments: json!({}),
                result: None,
                duration_ms: None,
                extra: None,
                kind: None,
                response: None,
            }]);
        }
        let expected = explicit_kind.as_deref().unwrap_or(match (source, has_tool_call) {
            ("user", _) => "dialogue",
            ("system", _) => "internal",
            ("agent", true) => "autonomous",
            _ => "dialogue",
        });
        prop_assert_eq!(turn.effective_kind(), expected);
    }
}
