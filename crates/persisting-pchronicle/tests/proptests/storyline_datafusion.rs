use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use persisting_pchronicle::storage::{
    StorylineContentReadMode, StorylineDataSource, StorylineDataSourceOptions, StorylineLanceStore,
};
use proptest::prelude::*;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build test runtime")
}

fn story() -> StorylineDocument {
    let mut story = StorylineDocument::new("session", "agent");
    story.turns.push(StorylineTurn {
        id: 1,
        kind: None,
        timestamp: None,
        source: "agent".into(),
        message: serde_json::json!("hello"),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: None,
        is_copied_context: None,
        latency_ms: Some(12),
        ttft_ms: Some(4),
        extra: None,
        env: None,
        prompt: None,
        finished_at: None,
    });
    story
}

fn story_with_timing(latency: i64, ttft: i64) -> StorylineDocument {
    let mut story = StorylineDocument::new("session", "agent");
    story.turns.push(StorylineTurn {
        id: 1,
        kind: None,
        timestamp: None,
        source: "agent".into(),
        message: serde_json::json!("hello"),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: None,
        is_copied_context: None,
        latency_ms: Some(latency),
        ttft_ms: Some(ttft),
        extra: None,
        env: None,
        prompt: None,
        finished_at: None,
    });
    story
}

proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 8,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn public_storyline_datasource_options_keep_normalized_columns_queryable(
        use_scalar_indexes in any::<bool>(),
        scan_in_order in any::<bool>(),
        preview in any::<bool>(),
    ) {
        let runtime = runtime();
        let temp = tempfile::tempdir().expect("create temporary Storyline root");
        let store = runtime
            .block_on(StorylineLanceStore::open(temp.path()))
            .expect("open Storyline store");
        runtime
            .block_on(store.replace_storyline(&story()))
            .expect("write Storyline fixture");

        let options = StorylineDataSourceOptions {
            use_scalar_indexes,
            scan_in_order,
            content_read_mode: if preview {
                StorylineContentReadMode::Preview
            } else {
                StorylineContentReadMode::Full
            },
        };
        let source = runtime
            .block_on(StorylineDataSource::from_store_with_options(&store, options))
            .expect("open pinned Storyline datasource");
        let context = datafusion::prelude::SessionContext::new();
        source.register(&context).expect("register Storyline tables");
        let batches = runtime.block_on(async {
            context
                .sql("SELECT latency, ttft FROM steps ORDER BY step_id")
                .await
                .expect("plan normalized timing columns")
                .collect()
                .await
                .expect("execute normalized timing columns")
        });
        prop_assert_eq!(batches.iter().map(|batch| batch.num_rows()).sum::<usize>(), 1);
        prop_assert_eq!(source.generation().is_empty(), false);
    }

    #[test]
    fn public_storyline_datasource_exposes_generated_timing_values(
        latency in 0i64..60_000,
        ttft_seed in 0i64..60_000,
    ) {
        let ttft = ttft_seed.min(latency);
        let runtime = runtime();
        let temp = tempfile::tempdir().expect("create temporary Storyline root");
        let store = runtime
            .block_on(StorylineLanceStore::open(temp.path()))
            .expect("open Storyline store");
        runtime
            .block_on(store.replace_storyline(&story_with_timing(latency, ttft)))
            .expect("write Storyline fixture");
        let source = runtime
            .block_on(StorylineDataSource::from_store_with_options(
                &store,
                StorylineDataSourceOptions::default(),
            ))
            .expect("open pinned Storyline datasource");
        let context = datafusion::prelude::SessionContext::new();
        source.register(&context).expect("register Storyline tables");
        let batches = runtime.block_on(async {
            context
                .sql("SELECT latency, ttft FROM steps ORDER BY step_id")
                .await
                .expect("plan timing query")
                .collect()
                .await
                .expect("execute timing query")
        });
        let row = &batches[0];
        let latency_array = row.column(0).as_any().downcast_ref::<datafusion::arrow::array::Int64Array>().unwrap();
        let ttft_array = row.column(1).as_any().downcast_ref::<datafusion::arrow::array::Int64Array>().unwrap();
        prop_assert_eq!(latency_array.value(0), latency);
        prop_assert_eq!(ttft_array.value(0), ttft);
    }
}
