use persisting_pchronicle::document::DocumentFormat;
use persisting_pchronicle::query::{ChronicleQueryEngine, ChronicleQueryExecutionOptions};
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
    fn public_atif_query_engine_returns_the_generated_session(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
    ) {
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": session,
            "agent": {"name": "agent", "version": "1"},
            "steps": []
        });
        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let path = temporary.path().join("generated.json");
        std::fs::write(&path, input.to_string()).expect("write ATIF input");
        let engine = runtime
            .block_on(ChronicleQueryEngine::open(
                DocumentFormat::Atif,
                &path,
                ChronicleQueryExecutionOptions::default(),
            ))
            .expect("open ATIF query engine");

        let backend = engine.backend_info().expect("backend info");
        prop_assert_eq!(backend.source_count, 1);
        prop_assert_eq!(backend.format, DocumentFormat::Atif);
        let rows = runtime
            .block_on(engine.query_jsonl("SELECT session_id FROM runs"))
            .expect("query runs");
        let rows = rows
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON row"))
            .collect::<Vec<_>>();
        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(&rows[0]["session_id"], &input["trajectory_id"]);
    }

    #[test]
    fn public_atif_query_engine_projects_generated_step_messages(
        session in proptest::string::string_regex("[A-Za-z0-9_-]{1,24}").unwrap(),
        message in proptest::string::string_regex("[A-Za-z0-9 .,!?_:/-]{0,96}").unwrap(),
    ) {
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": session,
            "agent": {"name": "agent", "version": "1"},
            "steps": [{"step_id": 1, "source": "user", "message": message}]
        });
        let runtime = runtime();
        let temporary = tempfile::tempdir().expect("create temporary directory");
        let path = temporary.path().join("generated.json");
        std::fs::write(&path, input.to_string()).expect("write ATIF input");
        let engine = runtime
            .block_on(ChronicleQueryEngine::open(
                DocumentFormat::Atif,
                &path,
                ChronicleQueryExecutionOptions::default(),
            ))
            .expect("open ATIF query engine");
        let output = runtime
            .block_on(engine.query_jsonl("SELECT session_id, message_json FROM steps ORDER BY step_id"))
            .expect("query ATIF steps");
        let rows = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSON row"))
            .collect::<Vec<_>>();
        prop_assert_eq!(rows.len(), 1);
        prop_assert_eq!(&rows[0]["session_id"], &input["trajectory_id"]);
        let expected_message_json = serde_json::to_string(&input["steps"][0]["message"]).unwrap();
        prop_assert_eq!(&rows[0]["message_json"], &serde_json::Value::String(expected_message_json));
    }
}
