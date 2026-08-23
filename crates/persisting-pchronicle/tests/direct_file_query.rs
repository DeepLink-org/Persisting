//! Direct OpenAI JSON and ACTF directory query tests.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use persisting_pchronicle::document::{detect_format, DocumentFormat};
use persisting_pchronicle::query::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, SOURCE_FILE_COLUMN,
};

mod support;

use support::fixture_path;

fn fixtures() -> PathBuf {
    fixture_path("import_roundtrip")
}

fn atif_fixtures() -> PathBuf {
    fixture_path("atif")
}

fn json_rows(output: &str) -> Result<Vec<serde_json::Value>> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[tokio::test]
async fn queries_one_openai_json_with_the_virtual_file_column() -> Result<()> {
    let input = fixtures().join("cybergym_0729001_trimmed.json");
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        &input,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    assert_eq!(
        engine
            .backend_info()
            .expect("document backend info")
            .source_count,
        1
    );

    let rows = json_rows(
        &engine
            .query_jsonl(
                "SELECT session_id, _file_ FROM runs \
                 WHERE _file_ LIKE 'cybergym_0729%' ORDER BY session_id",
            )
            .await?,
    )?;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.get(SOURCE_FILE_COLUMN).and_then(|value| value.as_str())
            == Some("cybergym_0729001_trimmed.json")
    }));

    // The virtual column is available on all three normalized query tables,
    // including an empty tool_calls result.
    engine.query("SELECT _file_ FROM steps LIMIT 1").await?;
    engine
        .query("SELECT _file_ FROM tool_calls LIMIT 1")
        .await?;
    Ok(())
}

#[tokio::test]
async fn auto_detects_and_queries_response_only_openai_rows() -> Result<()> {
    let temp = tempfile::NamedTempFile::with_suffix(".json")?;
    fs::write(
        temp.path(),
        r#"[{"session_id":"response-only","step_id":1,"response":{"role":"assistant","content":"done"}}]"#,
    )?;
    let input = fs::read_to_string(temp.path())?;
    assert_eq!(
        detect_format(Some(temp.path()), Some(&input))?,
        Some(DocumentFormat::OpenaiMsg)
    );
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let rows = json_rows(&engine.query_jsonl("SELECT session_id FROM runs").await?)?;
    assert_eq!(rows[0]["session_id"], "response-only");
    Ok(())
}

#[tokio::test]
async fn openai_directory_uses_relative_paths_for_like_narrowing() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let nested = temp.path().join("batch");
    fs::create_dir(&nested)?;
    fs::copy(
        fixtures().join("cybergym_07270003_trimmed.json"),
        temp.path().join("first.json"),
    )?;
    fs::copy(
        fixtures().join("cybergym_0729001_trimmed.json"),
        nested.join("second.json"),
    )?;

    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    assert_eq!(
        engine
            .backend_info()
            .expect("document backend info")
            .source_count,
        2
    );
    let rows = json_rows(
        &engine
            .query_jsonl(
                "SELECT session_id, _file_ FROM runs \
                 WHERE _file_ LIKE 'batch/%' ORDER BY session_id",
            )
            .await?,
    )?;
    assert_eq!(
        rows.iter()
            .map(|row| row["session_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["cyber-b", "cyber-c"]
    );
    assert!(rows
        .iter()
        .all(|row| row[SOURCE_FILE_COLUMN] == "batch/second.json"));
    Ok(())
}

#[tokio::test]
async fn actf_directory_can_be_narrowed_by_filename_wildcard() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::copy(
        fixtures().join("make-doom-for-mips_trimmed.actf.json"),
        temp.path().join("doom.actf.json"),
    )?;
    fs::copy(
        fixtures().join("protein-assembly_trimmed.actf.json"),
        temp.path().join("protein.actf.json"),
    )?;

    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Actf,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    assert_eq!(
        engine
            .backend_info()
            .expect("document backend info")
            .source_count,
        2
    );
    let rows = json_rows(
        &engine
            .query_jsonl(
                "SELECT session_id, _file_ FROM runs \
                 WHERE _file_ LIKE 'protein%'",
            )
            .await?,
    )?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][SOURCE_FILE_COLUMN], "protein.actf.json");
    Ok(())
}

#[tokio::test]
async fn file_like_filter_prunes_unmatched_files_before_they_are_opened() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::copy(
        fixtures().join("cybergym_07270003_trimmed.json"),
        temp.path().join("good.json"),
    )?;
    fs::write(temp.path().join("unmatched.json"), "not-json\n")?;

    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let rows = json_rows(
        &engine
            .query_jsonl(
                "SELECT session_id FROM runs WHERE _file_ LIKE 'good%' ORDER BY session_id",
            )
            .await?,
    )?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "cyber-a");

    let in_rows = json_rows(
        &engine
            .query_jsonl(
                "SELECT session_id FROM runs WHERE _file_ IN ('good.json') ORDER BY session_id",
            )
            .await?,
    )?;
    assert_eq!(in_rows, rows);

    let no_rows = engine
        .query_jsonl("SELECT session_id FROM runs WHERE _file_ = 'missing.json'")
        .await?;
    assert!(no_rows.is_empty());

    let error = engine
        .query("SELECT session_id FROM runs")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("unmatched.json"), "{error:#}");
    Ok(())
}

#[tokio::test]
async fn opened_file_fingerprint_fails_closed_after_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.json");
    fs::copy(fixtures().join("cybergym_07270003_trimmed.json"), &input)?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        &input,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    fs::write(&input, "not-json")?;
    let changed = engine.query("SELECT * FROM runs").await.unwrap_err();
    assert!(format!("{changed:#}").contains("changed after manifest"));
    Ok(())
}

#[tokio::test]
async fn multi_file_joins_require_the_file_key() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::copy(
        fixtures().join("cybergym_07270003_trimmed.json"),
        temp.path().join("one.json"),
    )?;
    fs::copy(
        fixtures().join("cybergym_07270003_trimmed.json"),
        temp.path().join("two.json"),
    )?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::OpenaiMsg,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let unsafe_join = engine
        .query(
            "SELECT * FROM steps s JOIN tool_calls t \
             ON s.session_id = t.session_id AND s.step_id = t.step_id",
        )
        .await
        .unwrap_err();
    assert!(format!("{unsafe_join:#}").contains("must include"));

    let disjunctive_join = engine
        .query(
            "SELECT * FROM steps s JOIN tool_calls t \
             ON s.session_id = t.session_id \
             AND (s._file_ = t._file_ OR 1 = 1)",
        )
        .await
        .unwrap_err();
    assert!(format!("{disjunctive_join:#}").contains("must include"));

    let implicit_join = engine
        .query(
            "SELECT * FROM steps s, tool_calls t \
             WHERE s.session_id = t.session_id AND s.step_id = t.step_id",
        )
        .await
        .unwrap_err();
    assert!(format!("{implicit_join:#}").contains("must include"));

    engine
        .query(
            "SELECT * FROM steps s JOIN tool_calls t \
             ON s._file_ = t._file_ \
             AND s.session_id = t.session_id AND s.step_id = t.step_id",
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn atif_steps_projection_matches_full_normalization_and_prunes_rows() -> Result<()> {
    let first: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("dialogue_10.json"),
    )?)?;
    let second: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("long_context_20.json"),
    )?)?;
    let temp = tempfile::tempdir()?;
    let ndjson = temp.path().join("input.ndjson");
    fs::write(
        &ndjson,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&first)?,
            serde_json::to_string(&second)?
        ),
    )?;
    let compatibility = temp.path().join("input-array.json");
    fs::write(
        &compatibility,
        serde_json::to_vec_pretty(&vec![first, second])?,
    )?;

    let projected = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        &ndjson,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let full = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        &compatibility,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let queries = [
        "SELECT COUNT(*) AS steps FROM steps",
        "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source",
        "SELECT step_id, source FROM steps \
         WHERE session_id = 'fixture-long_context_20' AND step_id BETWEEN 5 AND 15 \
         ORDER BY step_id",
        "SELECT step_id FROM steps WHERE source = 'agent' ORDER BY session_id, step_id",
        "SELECT run_id, session_id, step_id, kind, effective_kind, timestamp, source, \
                message_json, reasoning_content, reasoning_effort_json, metrics_json, \
                model_name, llm_call_count, is_copied_context, latency_ms, ttft_ms, \
                had_observation, extra_json \
         FROM steps WHERE session_id = 'fixture-dialogue_10' AND step_id = 5",
    ];
    for query in queries {
        assert_eq!(
            projected.query_jsonl(query).await?,
            full.query_jsonl(query).await?
        );
    }

    let metrics = projected.local_file_metrics().expect("local metrics");
    assert_eq!(metrics.projected_files, 5);
    assert_eq!(metrics.files_parsed, 5);
    assert!(metrics.documents_pruned >= 2);
    assert!(metrics.rows_pruned > 0);
    assert!(metrics.rows_emitted > 0);
    assert!(metrics.projected_arrow_bytes > 0);
    Ok(())
}

#[tokio::test]
async fn projected_atif_streams_ndjson_pretty_object_and_pretty_array() -> Result<()> {
    let first: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("dialogue_10.json"),
    )?)?;
    let mut second_template: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("long_context_20.json"),
    )?)?;
    second_template["unselected_root_payload"] = serde_json::Value::String("z".repeat(128 * 1024));
    second_template["scanner_escape_fixture"] =
        serde_json::Value::String(r#"quoted: " } ], { [ \\"#.into());
    let mut documents = vec![first.clone()];
    for copy in 0..8 {
        let mut second = second_template.clone();
        let session_id = format!("streaming-second-{copy}");
        second["session_id"] = serde_json::Value::String(session_id.clone());
        second["trajectory_id"] = serde_json::Value::String(session_id);
        documents.push(second);
    }

    let temp = tempfile::tempdir()?;
    let ndjson = temp.path().join("input.ndjson");
    let ndjson_content = documents
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n");
    fs::write(&ndjson, format!("{ndjson_content}\n"))?;
    let object = temp.path().join("input-object.json");
    fs::write(&object, serde_json::to_vec_pretty(&first)?)?;
    let array = temp.path().join("input-array.json");
    fs::write(&array, serde_json::to_vec_pretty(&documents)?)?;

    let sql = "SELECT session_id, step_id, source FROM steps ORDER BY session_id, step_id";
    let ndjson_engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        &ndjson,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let ndjson_rows = ndjson_engine.query_jsonl(sql).await?;
    let array_engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        &array,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    assert_eq!(array_engine.query_jsonl(sql).await?, ndjson_rows);

    let object_engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        &object,
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let object_rows = object_engine.query_jsonl(sql).await?;
    assert_eq!(json_rows(&object_rows)?.len(), 10);
    assert_eq!(json_rows(&ndjson_rows)?.len(), 170);

    let ndjson_metrics = ndjson_engine.local_file_metrics().expect("NDJSON metrics");
    assert_eq!(ndjson_metrics.projected_files, 1);
    assert_eq!(ndjson_metrics.streamed_records, 9);
    assert!(ndjson_metrics.source_bytes_read > 64 * 1024);
    assert!(
        ndjson_metrics.streaming_buffer_peak_bytes < ndjson_metrics.source_bytes_read,
        "input buffering must stay bounded below the source size: {ndjson_metrics:?}"
    );
    let array_metrics = array_engine.local_file_metrics().expect("array metrics");
    assert_eq!(array_metrics.projected_files, 1);
    assert_eq!(array_metrics.streamed_records, 9);
    assert!(array_metrics.source_bytes_read > 1024 * 1024);
    assert!(array_metrics.streaming_buffer_peak_bytes < array_metrics.source_bytes_read);
    let object_metrics = object_engine.local_file_metrics().expect("object metrics");
    assert_eq!(object_metrics.projected_files, 1);
    assert_eq!(object_metrics.streamed_records, 1);
    Ok(())
}

#[tokio::test]
async fn projected_atif_includes_embedded_subagent_steps_by_document_id() -> Result<()> {
    let input = serde_json::json!({
        "schema_version": "ATIF-v1.7",
        "session_id": "shared-run",
        "trajectory_id": "root",
        "agent": {"name": "root", "version": "1"},
        "steps": [{"step_id": 1, "source": "agent", "message": "root"}],
        "subagent_trajectories": [{
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "child",
            "agent": {"name": "child", "version": "1"},
            "steps": [{"step_id": 1, "source": "agent", "message": "child"}]
        }]
    });
    let file = tempfile::NamedTempFile::with_suffix(".json")?;
    fs::write(file.path(), serde_json::to_vec(&input)?)?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        file.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;

    let documents = engine
        .query_jsonl("SELECT document_id, session_id FROM runs ORDER BY document_id")
        .await?;
    assert_eq!(
        json_rows(&documents)?,
        vec![
            serde_json::json!({"document_id": "child", "session_id": "shared-run"}),
            serde_json::json!({"document_id": "root", "session_id": "shared-run"}),
        ]
    );

    let rows = engine
        .query_jsonl("SELECT document_id, session_id, message_json FROM steps ORDER BY document_id")
        .await?;
    assert_eq!(
        json_rows(&rows)?,
        vec![
            serde_json::json!({
                "document_id": "child",
                "session_id": "shared-run",
                "message_json": "\"child\""
            }),
            serde_json::json!({
                "document_id": "root",
                "session_id": "shared-run",
                "message_json": "\"root\""
            }),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn projected_atif_ignores_unselected_large_values_without_materializing_the_file(
) -> Result<()> {
    let mut trajectory: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("dialogue_10.json"),
    )?)?;
    trajectory["steps"][0]["message"] = serde_json::Value::String("x".repeat(2 * 1024 * 1024));
    trajectory["unselected_root_payload"] =
        serde_json::json!({"nested": "y".repeat(2 * 1024 * 1024)});
    let temp = tempfile::NamedTempFile::with_suffix(".json")?;
    fs::write(temp.path(), serde_json::to_vec_pretty(&trajectory)?)?;

    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        temp.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let rows = json_rows(
        &engine
            .query_jsonl("SELECT step_id FROM steps ORDER BY step_id")
            .await?,
    )?;
    assert_eq!(rows.len(), 10);
    let metrics = engine.local_file_metrics().expect("local metrics");
    assert!(metrics.source_bytes_read > 4 * 1024 * 1024);
    assert!(metrics.streaming_buffer_peak_bytes <= 64 * 1024);
    assert!(metrics.projected_arrow_bytes < 16 * 1024);
    Ok(())
}

#[tokio::test]
async fn projected_array_enforces_json_separators() -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        atif_fixtures().join("dialogue_10.json"),
    )?)?;
    let record = serde_json::to_string(&value)?;

    let missing_comma = tempfile::NamedTempFile::with_suffix(".json")?;
    fs::write(missing_comma.path(), format!("[{record} {record}]"))?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        missing_comma.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let error = engine
        .query("SELECT COUNT(*) FROM steps")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("expected ',' or ']'"));

    let trailing_comma = tempfile::NamedTempFile::with_suffix(".json")?;
    fs::write(trailing_comma.path(), format!("[{record},]"))?;
    let engine = ChronicleQueryEngine::open(
        DocumentFormat::Atif,
        trailing_comma.path(),
        ChronicleQueryExecutionOptions::default(),
    )
    .await?;
    let error = engine
        .query("SELECT COUNT(*) FROM steps")
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("element must be an object"));
    Ok(())
}
