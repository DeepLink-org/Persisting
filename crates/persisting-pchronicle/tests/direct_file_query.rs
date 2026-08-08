//! Direct OpenAI JSON and ACTF directory query tests.

use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use persisting_pchronicle::detect_local_query_manifest;
use persisting_pchronicle::store::{
    story_runs_arrow_schema, story_steps_arrow_schema, story_tool_calls_arrow_schema,
};
use persisting_pchronicle::{
    ChronicleFormat, ChronicleQueryBackend, ChronicleQueryEngine, FileTrajectoryDataSourceOptions,
    LocalQueryManifest, SOURCE_FILE_COLUMN,
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/import_roundtrip")
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
    let engine = ChronicleQueryEngine::open_openai_msg(&input)?;
    assert!(matches!(
        engine.backend(),
        ChronicleQueryBackend::OpenaiMsg { files: 1 }
    ));

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
    let manifest = detect_local_query_manifest(temp.path())?;
    assert_eq!(manifest.format(), ChronicleFormat::OpenaiMsg);
    let engine = ChronicleQueryEngine::open_local_manifest(manifest)?;
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

    let engine = ChronicleQueryEngine::open_openai_msg(temp.path())?;
    assert!(matches!(
        engine.backend(),
        ChronicleQueryBackend::OpenaiMsg { files: 2 }
    ));
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

    let engine = ChronicleQueryEngine::open_actf(temp.path())?;
    assert!(matches!(
        engine.backend(),
        ChronicleQueryBackend::Actf { files: 2 }
    ));
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

    let engine = ChronicleQueryEngine::open_openai_msg(temp.path())?;
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
async fn detected_manifest_is_the_exact_reader_file_set() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::copy(
        fixtures().join("cybergym_07270003_trimmed.json"),
        temp.path().join("first.json"),
    )?;
    let manifest = detect_local_query_manifest(temp.path())?;
    fs::write(temp.path().join("added-after-detection.json"), "not-json\n")?;

    let engine = ChronicleQueryEngine::open_local_manifest(manifest)?;
    let rows = json_rows(&engine.query_jsonl("SELECT session_id FROM runs").await?)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["session_id"], "cyber-a");
    Ok(())
}

#[test]
fn virtual_file_column_does_not_modify_physical_lance_schemas() {
    assert!(story_runs_arrow_schema()
        .field_with_name(SOURCE_FILE_COLUMN)
        .is_err());
    assert!(story_steps_arrow_schema()
        .field_with_name(SOURCE_FILE_COLUMN)
        .is_err());
    assert!(story_tool_calls_arrow_schema()
        .field_with_name(SOURCE_FILE_COLUMN)
        .is_err());
}

#[tokio::test]
async fn shared_cache_reuses_one_normalization_across_virtual_tables() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.json");
    fs::copy(fixtures().join("cybergym_07270003_trimmed.json"), &input)?;
    let manifest = LocalQueryManifest::for_format(&input, ChronicleFormat::OpenaiMsg)?;
    let engine = ChronicleQueryEngine::open_local_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            cache_files: 1,
            cache_bytes: 16 * 1024 * 1024,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )?;
    assert_eq!(
        json_rows(&engine.query_jsonl("SELECT * FROM runs").await?)?.len(),
        1
    );

    fs::write(&input, "not-json")?;
    assert!(!engine.query_jsonl("SELECT * FROM steps").await?.is_empty());
    let metrics = engine.local_file_metrics().expect("local metrics");
    assert_eq!(metrics.files_parsed, 1);
    assert!(metrics.cache_hits >= 1);
    assert!(metrics.source_bytes_read > 0);
    Ok(())
}

#[tokio::test]
async fn manifest_fingerprint_and_file_size_limits_fail_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let input = temp.path().join("input.json");
    fs::copy(fixtures().join("cybergym_07270003_trimmed.json"), &input)?;
    let manifest = LocalQueryManifest::for_format(&input, ChronicleFormat::OpenaiMsg)?;
    fs::write(&input, "not-json")?;
    let engine = ChronicleQueryEngine::open_local_manifest(manifest)?;
    let changed = engine.query("SELECT * FROM runs").await.unwrap_err();
    assert!(format!("{changed:#}").contains("changed after manifest"));

    let manifest = LocalQueryManifest::for_format(
        fixtures().join("cybergym_07270003_trimmed.json"),
        ChronicleFormat::OpenaiMsg,
    )?;
    let engine = ChronicleQueryEngine::open_local_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            max_file_bytes: 1,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )?;
    let oversized = engine.query("SELECT * FROM runs").await.unwrap_err();
    assert!(format!("{oversized:#}").contains("max_file_bytes"));
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
    let engine = ChronicleQueryEngine::open_openai_msg(temp.path())?;
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
