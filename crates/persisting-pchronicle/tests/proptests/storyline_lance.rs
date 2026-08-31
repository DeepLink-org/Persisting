use persisting_pchronicle::document::{DocumentFormat, open_document};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use persisting_pchronicle::storage::StorylineLanceStore;
use proptest::prelude::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
}

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 8,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn public_storyline_lance_roundtrip_preserves_generated_text(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        message in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
    ) {
        let mut story = StorylineDocument::new(session, "agent");
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
            timestamp: None,
            source: "agent".into(),
            message: serde_json::Value::String(message),
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

        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        runtime
            .block_on(StorylineLanceStore::open(temporary.path()))
            .and_then(|store| runtime.block_on(store.replace_storyline(&story)))
            .expect("write Storyline Lance store");
        let source = runtime
            .block_on(open_document(DocumentFormat::StorylineLance, temporary.path()))
            .expect("open Storyline Lance source");
        let restored = runtime
            .block_on(source.project_storylines())
            .expect("project Storyline Lance source");
        prop_assert_eq!(restored, vec![story]);
    }

    #[test]
    fn public_storyline_lance_roundtrip_preserves_generated_turn_order(
        messages in proptest::collection::vec(
            proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
            1..8,
        ),
    ) {
        let mut story = StorylineDocument::new("session", "agent");
        story.turns = messages
            .into_iter()
            .enumerate()
            .map(|(index, message)| StorylineTurn {
                id: (index + 1) as i64,
                kind: None,
                timestamp: None,
                source: if index % 2 == 0 { "user" } else { "agent" }.into(),
                message: serde_json::Value::String(message),
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
            })
            .collect();
        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        runtime
            .block_on(StorylineLanceStore::open(temporary.path()))
            .and_then(|store| runtime.block_on(store.replace_storyline(&story)))
            .expect("write Storyline Lance store");
        let source = runtime
            .block_on(open_document(DocumentFormat::StorylineLance, temporary.path()))
            .expect("open Storyline Lance source");
        let restored = runtime
            .block_on(source.project_storylines())
            .expect("project Storyline Lance source");
        prop_assert_eq!(&restored[0].turns, &story.turns);
    }
}
