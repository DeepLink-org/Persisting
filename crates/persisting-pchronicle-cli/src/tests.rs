use super::*;

#[tokio::test]
async fn trajectory_append_uses_persisting_events_protocol_directly() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let request = persisting_events::TrajectoryAppendRequest {
        storage: temporary.path().to_string_lossy().into_owned(),
        agent_id: "agent".into(),
        session_id: "session".into(),
        root_session_id: None,
        records: vec![persisting_events::EventRecord {
            identity: persisting_events::EventIdentity::default(),
            seq: 1,
            source: "test".into(),
            kind: "note".into(),
            timestamp: None,
            session_id: Some("session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: None,
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: serde_json::json!({"content": "hello"}),
        }],
    };
    let response: persisting_events::TrajectoryAppendResponse =
        crate::control::append_trajectory(request).await?;
    assert_eq!(response.accepted_records, 1);
    assert_eq!(response.status, "ok");
    Ok(())
}
use clap::CommandFactory;
use serde_json::Value;
use std::fs;

fn atif_fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif/dialogue_10.json")
}

fn example_dataset(format: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/data")
        .join(format)
}

fn example_source(format: &str) -> PathBuf {
    let filename = match format {
        "atif" => "support-ticket.json",
        "openai-messages" => "training.json",
        "actf" => "code-repair.actf.json",
        other => panic!("unknown example format: {other}"),
    };
    example_dataset(format).join(filename)
}

async fn append_canonical_note(storage: &std::path::Path) -> Result<()> {
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"canonical"}),
            }],
        )
        .await?;
    Ok(())
}

#[test]
fn command_tree_contains_the_product_commands() {
    let command = Cli::command();
    let names = command
        .get_subcommands()
        .map(|command| command.get_name())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "control", "onboard", "default", "ls", "status", "query", "analysis", "find", "import",
            "export", "project", "echo", "serve",
        ]
    );
    let ls = command
        .get_subcommands()
        .find(|command| command.get_name() == "ls")
        .unwrap();
    assert!(ls.get_all_aliases().any(|alias| alias == "list"));
    let project = command
        .get_subcommands()
        .find(|command| command.get_name() == "project")
        .unwrap();
    assert_eq!(
        project
            .get_subcommands()
            .map(|command| command.get_name())
            .collect::<Vec<_>>(),
        ["build", "status", "verify", "sync", "watch", "rebuild"]
    );
}

#[tokio::test]
async fn project_watch_emits_sync_and_verification_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq: 0,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"watch"}),
            }],
        )
        .await?;
    let events = persisting_pchronicle::storage::raw_event_lance_path(&coords)?;
    let projection = temp.path().join("storyline");
    persisting_pchronicle::storage::build_storyline_projection(
        events.to_string_lossy(),
        projection.to_string_lossy(),
        "events.lance",
    )
    .await?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "project",
        "watch",
        "--from",
        projection.to_str().unwrap(),
        "--source",
        events.to_str().unwrap(),
        "--iterations",
        "1",
        "--interval-seconds",
        "1",
        "--max-backoff-seconds",
        "1",
        "--verify-every",
        "1",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let event: Value = serde_json::from_slice(&stdout)?;
    assert!(event.get("schema_version").is_none());
    assert_eq!(event["status"], "ok");
    assert_eq!(event["sync"]["mode"], "noop");
    assert_eq!(event["verification"]["fresh"], true);
    assert!(String::from_utf8(stderr)?.contains("project_watch iteration=1 status=ok"));
    Ok(())
}

#[tokio::test]
async fn project_watch_labels_missing_projection_without_diagnostic_text() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    let source = persisting_pchronicle::storage::raw_event_lance_path(&coords)?;
    let projection = temp.path().join("missing-projection");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "project",
        "watch",
        "--from",
        projection.to_str().unwrap(),
        "--source",
        source.to_str().unwrap(),
        "--iterations",
        "1",
        "--interval-seconds",
        "1",
        "--max-backoff-seconds",
        "1",
        "--verify-every",
        "1",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let event: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(event["status"], "error");
    assert_eq!(event["code"], "not_found");
    assert_eq!(event["message"], "projection was not found");
    assert!(event.get("error").is_none());
    assert!(!event
        .to_string()
        .contains(source.to_string_lossy().as_ref()));
    Ok(())
}

#[tokio::test]
async fn project_verify_labels_stale_projection_as_conflict() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;
    let coords = persisting_pchronicle::storage::StoryCoords::new(
        storage.to_string_lossy(),
        "agent",
        "session",
        None,
    );
    let source = persisting_pchronicle::storage::raw_event_lance_path(&coords)?;
    let projection = temp.path().join("storyline");
    persisting_pchronicle::storage::build_storyline_projection(
        source.to_string_lossy(),
        projection.to_string_lossy(),
        "events.lance",
    )
    .await?;
    persisting_pchronicle::storage::RawEventLanceStore
        .append_events(
            &coords,
            &[persisting_pchronicle::model::EventRecord {
                identity: Default::default(),
                seq: 1,
                source: "test".into(),
                kind: "note".into(),
                timestamp: None,
                session_id: None,
                agent_id: None,
                parent_uuid: None,
                trace_id: None,
                call_id: None,
                subagent_id: None,
                parent_agent_id: None,
                branch: None,
                parent_call_id: None,
                payload: serde_json::json!({"content":"stale"}),
            }],
        )
        .await?;

    let cli = Cli::try_parse_from([
        "pchronicle",
        "project",
        "verify",
        "--from",
        projection.to_str().unwrap(),
        "--source",
        source.to_str().unwrap(),
    ])?;
    let mut stdout = Vec::new();
    let error = run(cli, false, &mut stdout, &mut Vec::new())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "conflict: projection verification is not fresh"
    );
    let verification: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(verification["fresh"], false);
    Ok(())
}

#[tokio::test]
async fn list_discovers_nested_sources_as_json() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir(temp.path().join("nested"))?;
    fs::write(
        temp.path().join("nested/trajectory.json"),
        r#"[{"session_id":"s1","step_id":0,"messages":[]}]"#,
    )?;
    fs::write(
        temp.path().join("trajectory.jsonl"),
        r#"{"schema_version":"ATIF-v1.4","session_id":"s2","steps":[],"agent":{"id":"a"}}"#,
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "ls",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert!(value.get("schema_version").is_none());
    assert_eq!(value["sources"].as_array().unwrap().len(), 2);
    assert_eq!(value["sources"][0]["source_path"], "nested/trajectory.json");
    assert_eq!(value["sources"][1]["source_path"], "trajectory.jsonl");
    assert!(String::from_utf8(stderr)?.contains("snapshot_id="));
    Ok(())
}

#[tokio::test]
async fn list_alias_and_table_output_work() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("trajectory.json"), "[]")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "list",
        temp.path().to_str().unwrap(),
        "--format",
        "table",
        "--physical",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;
    let output = String::from_utf8(stdout)?;
    assert!(output.contains("SOURCE"));
    assert!(output.contains("LAST MODIFIED"));
    assert!(output.contains("trajectory.json"));
    Ok(())
}

#[tokio::test]
async fn status_reports_exact_counts_as_json() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        atif_fixture().to_str().unwrap(),
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert!(value.get("schema_version").is_none());
    assert_eq!(value["status"], "ready");
    assert_eq!(value["counts_complete"], true);
    assert_eq!(value["sources"]["total"], 1);
    assert_eq!(value["sources"]["ready"], 1);
    assert_eq!(value["sources"]["error"], 0);
    assert_eq!(value["counts"]["runs"], 1);
    assert_eq!(value["counts"]["trajectories"], 1);
    assert_eq!(value["counts"]["steps"], 10);
    assert_eq!(value["counts"]["tool_calls"], 0);
    assert_eq!(value["counts"]["events"], 0);
    assert!(value["source_errors"].as_array().unwrap().is_empty());
    assert!(String::from_utf8(stderr)?.contains("counts_complete=true"));
    Ok(())
}

#[tokio::test]
async fn status_and_analysis_use_bounded_canonical_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let storage = temp.path().join("capture");
    append_canonical_note(&storage).await?;

    let status = Cli::try_parse_from([
        "pchronicle",
        "status",
        storage.to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "strict",
    ])?;
    let mut stdout = Vec::new();
    run(status, false, &mut stdout, &mut Vec::new()).await?;
    let status: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(status["counts_complete"], true);
    assert_eq!(status["counts"]["trajectories"], 1);
    assert_eq!(status["counts"]["steps"], 1);
    assert_eq!(status["counts"]["events"], 1);

    let analysis = Cli::try_parse_from([
        "pchronicle",
        "analysis",
        "overview",
        storage.to_str().unwrap(),
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(analysis, false, &mut stdout, &mut Vec::new()).await?;
    let overview: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(overview["trajectories"], 1);
    assert_eq!(overview["steps"], 1);
    Ok(())
}

#[tokio::test]
async fn status_reports_partial_counts_for_bad_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::copy(atif_fixture(), temp.path().join("valid.json"))?;
    fs::write(temp.path().join("broken.json"), "{not-json")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["status"], "degraded");
    assert_eq!(value["counts_complete"], false);
    assert_eq!(value["sources"]["total"], 2);
    assert_eq!(value["sources"]["ready"], 1);
    assert_eq!(value["sources"]["error"], 1);
    assert_eq!(value["counts"]["runs"], 1);
    assert_eq!(value["counts"]["steps"], 10);
    assert_eq!(value["source_errors"][0]["source_path"], "broken.json");
    Ok(())
}

#[tokio::test]
async fn status_strict_mode_rejects_bad_sources() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("broken.json"), "{not-json")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--errors",
        "strict",
    ])?;

    assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn status_report_mode_marks_an_unreadable_dataset_as_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("broken.json"), "{not-json")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        temp.path().to_str().unwrap(),
        "--format",
        "json",
        "--errors",
        "report",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["status"], "error");
    assert_eq!(value["sources"]["ready"], 0);
    assert_eq!(value["sources"]["error"], 1);
    let error = value["source_errors"][0]["error"].as_str().unwrap();
    assert!(error.contains("<dataset>/broken.json"));
    assert!(!error.contains(temp.path().to_str().unwrap()));
    Ok(())
}

#[tokio::test]
async fn status_table_marks_counts_as_exact() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "status",
        atif_fixture().to_str().unwrap(),
        "--format",
        "table",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;

    let output = String::from_utf8(stdout)?;
    assert!(output.contains("FIELD"));
    assert!(output.contains("ACCURACY"));
    assert!(output.contains("trajectories  1          exact"));
    assert!(output.contains("steps         10          exact"));
    Ok(())
}

#[tokio::test]
async fn status_rejects_zero_timeout() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "status", ".", "--timeout-seconds", "0"])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--timeout-seconds must be greater than zero"
    );
    Ok(())
}

#[tokio::test]
async fn query_reads_all_example_dataset_formats_as_jsonl() -> Result<()> {
    for (format, expected_runs) in [("atif", 1), ("openai-messages", 2), ("actf", 1)] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            example_dataset(format).to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(value["runs"], expected_runs, "format={format}");
        assert!(String::from_utf8(stderr)?.contains("datasets=dataset"));
    }
    Ok(())
}

#[tokio::test]
async fn query_preserves_selected_column_order_in_table_and_csv() -> Result<()> {
    let dataset = example_dataset("atif");
    for format in ["table", "csv"] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            dataset.to_str().unwrap(),
            "SELECT session_id, step_id, source FROM dataset.steps ORDER BY step_id",
            "--format",
            format,
        ])?;
        let mut stdout = Vec::new();
        run(cli, format == "table", &mut stdout, &mut Vec::new()).await?;
        let output = String::from_utf8(stdout)?;
        let header = output.lines().next().unwrap();
        if format == "table" {
            assert_eq!(
                header.split_whitespace().collect::<Vec<_>>(),
                ["session_id", "step_id", "source"]
            );
        } else {
            assert_eq!(header, "session_id,step_id,source");
        }
        assert!(output.contains("support-001"));
    }
    Ok(())
}

#[tokio::test]
async fn query_supports_named_cross_dataset_sql() -> Result<()> {
    let atif = format!("atif={}", example_dataset("atif").display());
    let openai = format!("openai={}", example_dataset("openai-messages").display());
    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        "--dataset",
        &atif,
        "--dataset",
        &openai,
        "SELECT (SELECT COUNT(*) FROM atif.runs) AS atif_runs, \
             (SELECT COUNT(*) FROM openai.runs) AS openai_runs",
        "--format",
        "jsonl",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;

    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["atif_runs"], 1);
    assert_eq!(value["openai_runs"], 2);
    Ok(())
}

#[tokio::test]
async fn query_writes_new_files_without_overwriting() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("runs.csv");
    let dataset = example_dataset("actf");
    let args = [
        "pchronicle",
        "query",
        dataset.to_str().unwrap(),
        "SELECT session_id FROM dataset.runs",
        "--format",
        "csv",
        "--output",
        output.to_str().unwrap(),
    ];
    let cli = Cli::try_parse_from(args)?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    assert!(stdout.is_empty());
    assert_eq!(
        fs::read_to_string(&output)?,
        "session_id\nexample-code-repair\n"
    );

    let cli = Cli::try_parse_from(args)?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("create query output file"));
    Ok(())
}

#[tokio::test]
async fn query_rejects_writes_and_bounded_output_without_partial_stdout() -> Result<()> {
    for (sql, limit_flag, limit, expected) in [
        (
            "DELETE FROM dataset.runs",
            "--max-output-rows",
            "100",
            "only accepts SELECT",
        ),
        (
            "SELECT * FROM dataset.steps",
            "--max-output-rows",
            "1",
            "max_output_rows",
        ),
        (
            "SELECT * FROM dataset.steps",
            "--max-output-bytes",
            "8",
            "max_output_bytes",
        ),
    ] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            example_dataset("atif").to_str().unwrap(),
            sql,
            limit_flag,
            limit,
        ])?;
        let mut stdout = Vec::new();
        let error = run(cli, false, &mut stdout, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{error:#}");
        assert!(stdout.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn query_errors_preserve_the_operational_source_chain() -> Result<()> {
    let dataset = example_dataset("atif");
    let sql = "SELECT secret_column FROM dataset.runs";
    let cli = Cli::try_parse_from(["pchronicle", "query", dataset.to_str().unwrap(), sql])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        error.chain().count() >= 2,
        "missing source chain: {error:#}"
    );
    Ok(())
}

#[tokio::test]
async fn query_rejects_malformed_named_dataset_mounts() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "query",
        "--dataset",
        "missing-separator",
        "SELECT 1",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("NAME=URI"));
    Ok(())
}

#[tokio::test]
async fn find_locates_runs_sessions_and_steps_in_example_datasets() -> Result<()> {
    for (format, flag, identity, expected_source) in [
        ("atif", "--session-id", "support-001", "support-ticket.json"),
        (
            "actf",
            "--run-id",
            "example-code-repair",
            "code-repair.actf.json",
        ),
        (
            "openai-messages",
            "--session-id",
            "training-002",
            "training.json",
        ),
    ] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset(format).to_str().unwrap(),
            flag,
            identity,
            "--format",
            "json",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;

        let value: Value = serde_json::from_slice(&stdout)?;
        assert!(value.get("schema_version").is_none());
        assert_eq!(value["truncated"], false);
        assert_eq!(value["matches"][0]["source_path"], expected_source);
    }

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "support-001",
        "--step-id",
        "2",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"][0]["step_id"], 2);
    assert_eq!(value["matches"][0]["step_source"], "agent");
    assert_eq!(value["matches"][0]["effective_kind"], "autonomous");
    Ok(())
}

#[tokio::test]
async fn find_discovers_candidates_and_source_narrows_them() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for file in ["first.json", "second.json"] {
        fs::write(
            temp.path().join(file),
            r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
        )?;
    }

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--session-id",
        "shared",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"].as_array().unwrap().len(), 2);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--source",
        "second.json",
        "--session-id",
        "shared",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    assert_eq!(value["matches"][0]["source_path"], "second.json");
    Ok(())
}

#[tokio::test]
async fn find_reports_truncation_and_empty_results() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("openai-messages").to_str().unwrap(),
        "--document-id",
        "training-001",
        "--max-results",
        "1",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["truncated"], false);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "missing",
        "--format",
        "table",
    ])?;
    let mut stdout = Vec::new();
    run(cli, true, &mut stdout, &mut Vec::new()).await?;
    assert!(String::from_utf8(stdout)?.contains("(0 matches)"));
    Ok(())
}

#[tokio::test]
async fn find_truncates_ambiguous_candidates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for file in ["a.json", "b.json"] {
        fs::write(
            temp.path().join(file),
            r#"[{"id":"event","session_id":"shared","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
        )?;
    }
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--session-id",
        "shared",
        "--max-results",
        "1",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["truncated"], true);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    Ok(())
}

#[tokio::test]
async fn find_validates_source_paths_and_escapes_quotes() -> Result<()> {
    for source in ["/absolute.json", "../outside.json", "s3://bucket/file"] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--source",
            source,
            "--session-id",
            "support-001",
        ])?;
        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
    }

    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join("it's-valid.json"),
        r#"[{"id":"event","session_id":"quoted","step_id":1,"messages":[],"response":{"role":"assistant","content":"ok"}}]"#,
    )?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        temp.path().to_str().unwrap(),
        "--source",
        "it's-valid.json",
        "--session-id",
        "quoted",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let value: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(value["matches"][0]["source_path"], "it's-valid.json");
    Ok(())
}

#[tokio::test]
async fn find_enforces_output_byte_limit_without_partial_stdout() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "find",
        example_dataset("atif").to_str().unwrap(),
        "--session-id",
        "support-001",
        "--max-output-bytes",
        "8",
        "--format",
        "json",
    ])?;
    let mut stdout = Vec::new();
    let error = run(cli, false, &mut stdout, &mut Vec::new())
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("max_output_bytes"),
        "{error:#}"
    );
    assert!(stdout.is_empty());
    Ok(())
}

#[test]
fn find_cli_requires_one_identity_and_session_for_steps() {
    assert!(Cli::try_parse_from(["pchronicle", "find", "."]).is_err());
    assert!(Cli::try_parse_from([
        "pchronicle",
        "find",
        ".",
        "--run-id",
        "r",
        "--session-id",
        "s"
    ])
    .is_err());
    assert!(Cli::try_parse_from(["pchronicle", "find", ".", "--step-id", "1"]).is_err());
}

#[tokio::test]
async fn find_rejects_empty_and_oversized_identities() -> Result<()> {
    for identity in ["", &"x".repeat(4097)] {
        let cli = Cli::try_parse_from([
            "pchronicle",
            "find",
            example_dataset("atif").to_str().unwrap(),
            "--session-id",
            identity,
        ])?;
        assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
            .await
            .is_err());
    }
    Ok(())
}

#[tokio::test]
async fn import_creates_queryable_lossless_datasets_for_all_example_formats() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for (format, expected_format, source_name, expected_runs) in [
        ("atif", "atif", "trajectories.atif.json", 1),
        ("openai-messages", "openai-msg", "session_steps.json", 2),
        ("actf", "actf", "trajectories.actf.json", 1),
    ] {
        let input = example_source(format);
        let output = temp.path().join(format);
        let cli = Cli::try_parse_from([
            "pchronicle",
            "import",
            "--from",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ])?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run(cli, false, &mut stdout, &mut stderr).await?;

        let response: Value = serde_json::from_slice(&stdout)?;
        assert!(response.get("schema_version").is_none());
        assert_eq!(response["format"], expected_format);
        assert_eq!(response["source_path"], source_name);
        assert_eq!(response["trajectories"], expected_runs);
        assert_eq!(
            fs::read(output.join(source_name))?,
            fs::read(&input)?,
            "import must preserve the exchange document byte-for-byte"
        );
        assert!(String::from_utf8(stderr)?.contains("trajectories="));

        let cli = Cli::try_parse_from([
            "pchronicle",
            "query",
            output.to_str().unwrap(),
            "SELECT COUNT(*) AS runs FROM dataset.runs",
            "--format",
            "jsonl",
        ])?;
        let mut stdout = Vec::new();
        run(cli, false, &mut stdout, &mut Vec::new()).await?;
        let count: Value = serde_json::from_slice(&stdout)?;
        assert_eq!(count["runs"], expected_runs, "format={format}");
    }
    Ok(())
}

#[tokio::test]
async fn import_reads_a_bounded_explicit_stdin_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("streamed");
    let input = fs::read(example_source("atif"))?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        "-",
        "--stream",
        "--format",
        "atif",
        "--output",
        output.to_str().unwrap(),
        "--max-input-bytes",
        &input.len().to_string(),
    ])?;
    let mut stdin = input.as_slice();
    let mut stdout = Vec::new();
    run_with_stdin(cli, false, &mut stdin, &mut stdout, &mut Vec::new()).await?;
    let response: Value = serde_json::from_slice(&stdout)?;
    assert_eq!(response["trajectories"], 1);
    assert_eq!(fs::read(output.join("trajectories.atif.json"))?, input);

    for args in [
        vec![
            "pchronicle",
            "import",
            "--from",
            "-",
            "--output",
            temp.path().join("missing-stream").to_str().unwrap(),
            "--format",
            "atif",
        ],
        vec![
            "pchronicle",
            "import",
            "--from",
            "-",
            "--stream",
            "--output",
            temp.path().join("missing-format").to_str().unwrap(),
        ],
    ] {
        let cli = Cli::try_parse_from(args)?;
        let mut stdin = input.as_slice();
        assert!(
            run_with_stdin(cli, false, &mut stdin, &mut Vec::new(), &mut Vec::new())
                .await
                .is_err()
        );
    }
    Ok(())
}

#[tokio::test]
async fn import_rejects_invalid_oversized_and_unsupported_input_without_partial_output(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let invalid = temp.path().join("invalid.json");
    fs::write(&invalid, "not json")?;

    for (name, extra, code) in [
        ("invalid", vec![], "invalid_request"),
        (
            "oversized",
            vec!["--max-input-bytes", "1"],
            "resource_exhausted",
        ),
        ("storyline", vec!["--format", "storyline"], "unsupported"),
    ] {
        let output = temp.path().join(name);
        let mut args = vec![
            "pchronicle",
            "import",
            "--from",
            invalid.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
        ];
        args.extend(extra);
        let cli = Cli::try_parse_from(args)?;
        let mut stdout = Vec::new();
        let error = run(cli, false, &mut stdout, &mut Vec::new())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").starts_with(code), "{error:#}");
        assert!(stdout.is_empty());
        assert!(!output.exists());
    }
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));
    Ok(())
}

#[tokio::test]
async fn import_is_create_only_and_rejects_duplicate_documents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("existing");
    fs::create_dir(&output)?;
    fs::write(output.join("sentinel"), "keep")?;
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        example_source("atif").to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ])?;
    assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .is_err());
    assert_eq!(fs::read_to_string(output.join("sentinel"))?, "keep");

    let trajectory: Value = serde_json::from_slice(&fs::read(example_source("atif"))?)?;
    let duplicate_input = temp.path().join("duplicates.json");
    fs::write(
        &duplicate_input,
        serde_json::to_vec(&serde_json::json!([trajectory.clone(), trajectory]))?,
    )?;
    let duplicate_output = temp.path().join("duplicates");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        duplicate_input.to_str().unwrap(),
        "--output",
        duplicate_output.to_str().unwrap(),
        "--format",
        "atif",
    ])?;
    let error = run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid_request: import contains duplicate document_id"
    );
    assert!(!duplicate_output.exists());
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-import-"))
    }));

    let mut first: Value = serde_json::from_slice(&fs::read(example_source("atif"))?)?;
    first["trajectory_id"] = serde_json::json!("document-a");
    let mut second = first.clone();
    second["trajectory_id"] = serde_json::json!("document-b");
    let shared_input = temp.path().join("shared-session.json");
    fs::write(
        &shared_input,
        serde_json::to_vec(&serde_json::json!([first, second]))?,
    )?;
    let shared_output = temp.path().join("shared-session");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "import",
        "--from",
        shared_input.to_str().unwrap(),
        "--output",
        shared_output.to_str().unwrap(),
        "--format",
        "atif",
    ])?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(shared_output.is_dir());
    Ok(())
}

#[tokio::test]
async fn import_validation_does_not_inherit_the_materialization_row_limit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("large-valid.atif.json");
    let steps = (1..=10_000)
        .map(|step_id| {
            serde_json::json!({
                "step_id": step_id,
                "source": "agent",
                "message": "ok"
            })
        })
        .collect::<Vec<_>>();
    fs::write(
        &input,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "large-document",
            "session_id": "large-session",
            "agent": {"name": "test", "version": "1"},
            "steps": steps
        }))?,
    )?;

    assert_eq!(
        exchange::validate_import_source(ExchangeFormat::Atif, &input).await?,
        1
    );
    Ok(())
}

#[test]
fn import_publish_primitive_never_replaces_an_existing_target() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let staged = temp.path().join("staged");
    let existing = temp.path().join("existing");
    fs::create_dir(&staged)?;
    fs::create_dir(&existing)?;
    fs::write(staged.join("new"), "new")?;
    fs::write(existing.join("sentinel"), "keep")?;

    assert!(rename_noreplace(&staged, &existing).is_err());
    assert_eq!(fs::read_to_string(existing.join("sentinel"))?, "keep");
    assert_eq!(fs::read_to_string(staged.join("new"))?, "new");
    Ok(())
}

#[tokio::test]
async fn export_filters_complete_trajectories_and_streams_finite_json() -> Result<()> {
    let dataset = example_dataset("openai-messages");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        dataset.to_str().unwrap(),
        "--output",
        "-",
        "--stream",
        "--format",
        "openai-messages",
        "--session-id",
        "training-002",
        "--where",
        "step_count = 2",
    ])?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run(cli, false, &mut stdout, &mut stderr).await?;

    let rows: Value = serde_json::from_slice(&stdout)?;
    let rows = rows.as_array().context("OpenAI export must be an array")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "training-002");
    assert!(String::from_utf8(stderr)?.contains("trajectories=1"));
    Ok(())
}

#[tokio::test]
async fn export_converts_complete_trajectories_between_formats() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        "-",
        "--stream",
        "--format",
        "storyline",
        "--session-id",
        "support-001",
    ])?;
    let mut stdout = Vec::new();
    run(cli, false, &mut stdout, &mut Vec::new()).await?;
    let story: persisting_pchronicle::model::StorylineDocument = serde_json::from_slice(&stdout)?;
    assert_eq!(story.session_id, "support-001");
    assert_eq!(story.turns.len(), 3);
    Ok(())
}

#[tokio::test]
async fn export_is_bounded_create_only_and_has_no_partial_output() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("export.json");
    let dataset = example_dataset("atif");
    fs::write(&output, "sentinel")?;
    let base = [
        "pchronicle",
        "export",
        "--from",
        dataset.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--format",
        "atif",
    ];
    let cli = Cli::try_parse_from(base)?;
    assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .is_err());
    assert_eq!(fs::read_to_string(&output)?, "sentinel");

    let mut overwrite = base.to_vec();
    overwrite.push("--overwrite");
    let cli = Cli::try_parse_from(overwrite)?;
    run(cli, false, &mut Vec::new(), &mut Vec::new()).await?;
    assert!(fs::read_to_string(&output)?.contains("support-001"));

    let limited = temp.path().join("limited.json");
    let cli = Cli::try_parse_from([
        "pchronicle",
        "export",
        "--from",
        example_dataset("atif").to_str().unwrap(),
        "--output",
        limited.to_str().unwrap(),
        "--format",
        "atif",
        "--max-output-bytes",
        "8",
    ])?;
    assert!(run(cli, false, &mut Vec::new(), &mut Vec::new())
        .await
        .is_err());
    assert!(!limited.exists());
    assert!(!fs::read_dir(temp.path())?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.starts_with(".pchronicle-export-"))
    }));
    Ok(())
}

#[tokio::test]
async fn export_validates_stream_filters_and_strict_conversion() -> Result<()> {
    for args in [
        vec![
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--output",
            "-",
            "--format",
            "atif",
        ],
        vec![
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--output",
            "-",
            "--stream",
            "--format",
            "storyline",
            "--strict",
        ],
        vec![
            "pchronicle",
            "export",
            "--from",
            example_dataset("atif").to_str().unwrap(),
            "--output",
            "-",
            "--stream",
            "--format",
            "atif",
            "--where",
            "DELETE FROM dataset.runs",
        ],
    ] {
        let cli = Cli::try_parse_from(args)?;
        let mut stdout = Vec::new();
        assert!(run(cli, false, &mut stdout, &mut Vec::new()).await.is_err());
        assert!(stdout.is_empty());
    }
    Ok(())
}

#[test]
fn rejects_credentials_and_signed_queries() {
    assert!(normalize_and_validate_dataset_uri("s3://user:secret@bucket/path").is_err());
    assert!(normalize_and_validate_dataset_uri("s3://bucket/path?X-Amz-Signature=secret").is_err());
    assert!(normalize_and_validate_dataset_uri("https://example.com/data").is_err());
}

#[test]
fn preserves_uri_roots_while_trimming_prefixes() {
    assert_eq!(
        normalize_and_validate_dataset_uri("file:///").unwrap(),
        "file:///"
    );
    assert_eq!(
        normalize_and_validate_dataset_uri("s3://bucket///").unwrap(),
        "s3://bucket"
    );
}

#[test]
fn warehouse_config_normalizes_mounts_and_selects_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    fs::create_dir_all(&first)?;
    fs::create_dir_all(&second)?;
    let config_path = temp.path().join("warehouse.toml");
    fs::write(
        &config_path,
        format!(
            r#"
default_dataset = "archive"

[[datasets]]
name = "live"
uri = {first:?}

[[datasets]]
name = "archive"
uri = {second:?}
"#,
            first = first.to_string_lossy(),
            second = second.to_string_lossy(),
        ),
    )?;

    let config = load_warehouse_config(&config_path)?;
    assert_eq!(config.datasets.len(), 2);
    assert_eq!(config.default_dataset.as_deref(), Some("archive"));
    assert_eq!(
        config.catalog_options.error_policy,
        CatalogErrorPolicy::Report
    );
    assert_eq!(
        config.datasets[0].uri,
        fs::canonicalize(first)?.to_string_lossy()
    );
    assert_eq!(
        config.datasets[1].uri,
        fs::canonicalize(second)?.to_string_lossy()
    );
    Ok(())
}

#[test]
fn warehouse_config_rejects_unsafe_or_ambiguous_mounts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let dataset = temp.path().join("dataset");
    fs::create_dir(&dataset)?;

    for (name, body, expected) in [
            (
                "duplicate.toml",
                format!(
                    "[[datasets]]\nname='live'\nuri={dataset:?}\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "unique",
            ),
            (
                "missing-default.toml",
                format!(
                    "default_dataset='missing'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "not mounted",
            ),
            (
                "credential.toml",
                "[[datasets]]\nname='live'\nuri='s3://user:secret@bucket/path'\n".into(),
                "credentials",
            ),
            (
                "unknown.toml",
                format!(
                    "listen='0.0.0.0:80'\n[[datasets]]\nname='live'\nuri={dataset:?}\n",
                    dataset = dataset.to_string_lossy()
                ),
                "unknown field",
            ),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, body)?;
            let error = load_warehouse_config(&path).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "unexpected error for {name}: {error:#}"
            );
        }
    Ok(())
}

#[test]
fn serve_cli_defaults_to_loopback_and_rejects_public_listeners() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "serve", "--config", "warehouse.toml"])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(args.listen, "127.0.0.1:8080".parse::<SocketAddr>()?);

    let cli = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--config",
        "warehouse.toml",
        "--listen",
        "0.0.0.0:8080",
    ])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert!(!args.listen.ip().is_loopback());
    Ok(())
}

#[test]
fn echo_cli_uses_a_normal_loopback_default() -> Result<()> {
    let cli = Cli::try_parse_from(["pchronicle", "echo"])?;
    let Command::Echo(args) = cli.command else {
        unreachable!("echo command parsed as another variant")
    };
    assert_eq!(args.listen, "127.0.0.1:19080".parse::<SocketAddr>()?);
    assert_eq!(args.encoding, EchoEncoding::Plain);
    Ok(())
}

#[test]
fn serve_gateway_options_are_explicit_and_scoped() -> Result<()> {
    let cli = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--config",
        "warehouse.toml",
        "--gateway",
        "gateway.toml",
        "--gateway-dataset",
        "captures",
        "--gateway-state",
        ".gateway-state",
        "--gateway-stream-markdown",
        "--debug",
    ])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert_eq!(args.gateway, Some(PathBuf::from("gateway.toml")));
    assert_eq!(args.gateway_dataset.as_deref(), Some("captures"));
    assert_eq!(args.gateway_state, Some(PathBuf::from(".gateway-state")));
    assert!(args.gateway_stream_markdown);
    assert!(args.debug);

    assert!(Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--config",
        "warehouse.toml",
        "--gateway-dataset",
        "captures",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--config",
        "warehouse.toml",
        "--debug",
    ])
    .is_err());

    let cli = Cli::try_parse_from([
        "pchronicle",
        "serve",
        "--config",
        "warehouse.toml",
        "--gateway-config",
        "gateway.toml",
        "--gateway-debug",
    ])?;
    let Command::Serve(args) = cli.command else {
        unreachable!("serve command parsed as another variant")
    };
    assert!(args.debug);
    Ok(())
}

#[test]
fn gateway_dataset_selection_uses_only_static_mounts() -> Result<()> {
    let captures = DatasetMount::new("captures", "/tmp/captures")?;
    let evals = DatasetMount::new("evals", "/tmp/evals")?;
    let mut config = server::ChronicleServerConfig::mounted(vec![captures, evals])?;
    assert!(select_gateway_dataset(&config, None)
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    assert_eq!(
        select_gateway_dataset(&config, Some("captures"))?.name,
        "captures"
    );
    assert!(select_gateway_dataset(&config, Some("missing"))
        .unwrap_err()
        .to_string()
        .contains("not mounted"));

    config.default_dataset = Some("evals".into());
    assert_eq!(select_gateway_dataset(&config, None)?.name, "evals");
    Ok(())
}

#[test]
fn embedded_gateway_rejects_public_listeners() {
    let error = parse_gateway_listener("0.0.0.0:8787", "Gateway").unwrap_err();
    assert!(error.to_string().contains("loopback"));
    assert!(parse_gateway_listener("127.0.0.1:0", "Gateway").is_ok());
}

#[tokio::test]
async fn embedded_gateway_forwards_and_persists_canonical_events() -> Result<()> {
    let dataset = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_addr = upstream_listener.local_addr()?;
    let (upstream_stop_tx, upstream_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let upstream = tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({
                    "id": "chatcmpl-pchronicle",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "stored"},
                        "finish_reason": "stop"
                    }]
                }))
            }),
        );
        axum::serve(upstream_listener, app)
            .with_graceful_shutdown(async {
                let _ = upstream_stop_rx.await;
            })
            .await
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let gateway_addr = listener.local_addr()?;
    let admin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let admin_addr = admin_listener.local_addr()?;
    let mut config = persisting_gateway::config::ProxyConfig::from_toml_str(&format!(
        r#"
listen = "127.0.0.1:0"
admin_listen = "127.0.0.1:0"
agent_id = "test-agent"

[[models]]
name = "*"
upstream = "http://{upstream_addr}/v1"
"#
    ))?;
    config.listen = gateway_addr.to_string();
    config.admin_listen = admin_addr.to_string();
    let (sink, writer) =
        gateway_capture::gateway_capture_sink(&dataset.path().to_string_lossy(), &config.agent_id)?;
    let warehouse_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let warehouse_addr = warehouse_listener.local_addr()?;
    let warehouse_config = server::ChronicleServerConfig::mounted(vec![DatasetMount::default(
        dataset.path().to_string_lossy(),
    )?])?;
    let prepared_gateway = PreparedGateway {
        config,
        state_dir: state.path().to_path_buf(),
        dataset_name: DEFAULT_DATASET_NAME.into(),
        stream_markdown: false,
        listener,
        admin_listener,
        sink,
        writer,
    };
    let (serve_stop_tx, serve_stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = tokio::spawn(async move {
        serve_warehouse_and_gateway(
            warehouse_config,
            warehouse_listener,
            prepared_gateway,
            async {
                let _ = serve_stop_rx.await;
            },
        )
        .await
    });

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let health = client
        .get(format!("http://{warehouse_addr}/api/health"))
        .send()
        .await?;
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    assert_eq!(health.json::<Value>().await?["mode"], "read_only");
    let response = client
        .post(format!("http://{gateway_addr}/v1/chat/completions"))
        .header("content-type", "application/json")
        .header("x-persisting-session-id", "session-42")
        .body(r#"{"model":"test","messages":[{"role":"user","content":"keep me"}]}"#)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await?["choices"][0]["message"]["content"],
        "stored"
    );

    let _ = serve_stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), serve)
        .await
        .context("pChronicle serve shutdown timed out")???;
    let _ = upstream_stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), upstream)
        .await
        .context("mock upstream shutdown timed out")???;

    let snapshot = Arc::new(
        DatasetCatalogSnapshot::discover(
            vec![DatasetMount::default(dataset.path().to_string_lossy())?],
            Some(DEFAULT_DATASET_NAME.into()),
            CatalogSnapshotOptions::default(),
        )
        .await?,
    );
    let engine = snapshot.query_engine(Default::default()).await?;
    let rows = engine
        .query_jsonl(
            "SELECT kind, COUNT(*) AS count FROM dataset.events GROUP BY kind ORDER BY kind",
        )
        .await?;
    let rows = rows
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["kind"], "llm.request");
    assert_eq!(rows[0]["count"], 1);
    assert_eq!(rows[1]["kind"], "llm.response");
    assert_eq!(rows[1]["count"], 1);
    Ok(())
}

#[test]
fn byte_units_are_stable() {
    assert_eq!(format_bytes(12), "12 B");
    assert_eq!(format_bytes(1536), "1.5 KiB");
}
