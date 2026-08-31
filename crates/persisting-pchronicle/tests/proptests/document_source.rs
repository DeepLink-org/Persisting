use persisting_pchronicle::document::{DocumentFormat, encode_agenticmd, open_document};
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
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
    fn public_agenticmd_document_source_projects_generated_story(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        message in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
    ) {
        let mut story = StorylineDocument::new(session, "agent");
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
            timestamp: None,
            source: "user".into(),
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
        let path = temporary.path().join("generated.md");
        std::fs::write(&path, encode_agenticmd(&story).expect("encode AgenticMD"))
            .expect("write AgenticMD");
        let source = runtime
            .block_on(open_document(DocumentFormat::AgenticMd, &path))
            .expect("open AgenticMD source");
        let projected = runtime
            .block_on(source.project_storylines())
            .expect("project AgenticMD source");
        prop_assert_eq!(projected, vec![story]);
    }

    #[test]
    fn public_agenticmd_document_source_streams_each_storyline_once(
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
        let path = temporary.path().join("generated.md");
        std::fs::write(&path, encode_agenticmd(&story).expect("encode AgenticMD"))
            .expect("write AgenticMD");
        let source = runtime
            .block_on(open_document(DocumentFormat::AgenticMd, &path))
            .expect("open AgenticMD source");
        let mut streamed = Vec::new();
        runtime
            .block_on(source.for_each_storyline(|storyline| {
                streamed.push(storyline);
                Ok(())
            }))
            .expect("stream AgenticMD source");
        prop_assert_eq!(streamed, vec![story]);
    }
}
