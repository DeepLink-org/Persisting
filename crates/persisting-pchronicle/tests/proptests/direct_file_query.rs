use persisting_pchronicle::document::DocumentFormat;
use persisting_pchronicle::query::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, SOURCE_FILE_COLUMN,
};
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
    fn public_openai_file_queries_expose_its_relative_file_key(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        message in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{1,96}").unwrap(),
    ) {
        let input = serde_json::json!([{
            "session_id": session,
            "step_id": 1,
            "response": {"role": "assistant", "content": message}
        }]);
        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let path = temporary.path().join("generated.json");
        std::fs::write(&path, input.to_string()).expect("write OpenAI input");
        let engine = runtime
            .block_on(ChronicleQueryEngine::open(
                DocumentFormat::OpenaiMsg,
                &path,
                ChronicleQueryExecutionOptions::default(),
            ))
            .expect("open OpenAI query engine");
        let rows = runtime
            .block_on(engine.query_jsonl("SELECT session_id, _file_ FROM runs"))
            .expect("query OpenAI runs");
        let rows = rows
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON row"))
            .collect::<Vec<_>>();
        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(&rows[0]["session_id"], &input[0]["session_id"]);
        prop_assert_eq!(&rows[0][SOURCE_FILE_COLUMN], &serde_json::Value::String("generated.json".into()));
    }

    #[test]
    fn public_openai_file_queries_preserve_one_run_per_generated_row(
        rows in proptest::collection::vec(
            (
                proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
                proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{1,96}").unwrap(),
            ),
            1..8,
        ),
    ) {
        let input = rows.iter().map(|(session, message)| serde_json::json!({
            "session_id": session,
            "step_id": 1,
            "response": {"role": "assistant", "content": message},
        })).collect::<Vec<_>>();
        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let path = temporary.path().join("generated.json");
        std::fs::write(&path, serde_json::Value::Array(input).to_string()).expect("write OpenAI input");
        let engine = runtime
            .block_on(ChronicleQueryEngine::open(
                DocumentFormat::OpenaiMsg,
                &path,
                ChronicleQueryExecutionOptions::default(),
            ))
            .expect("open OpenAI query engine");
        let output = runtime
            .block_on(engine.query_jsonl("SELECT session_id, _file_ FROM runs ORDER BY session_id"))
            .expect("query OpenAI runs");
        let actual = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON row"))
            .collect::<Vec<_>>();
        prop_assert_eq!(actual.len(), rows.len());
        prop_assert!(actual.iter().all(|row| row[SOURCE_FILE_COLUMN] == "generated.json"));
    }
}
