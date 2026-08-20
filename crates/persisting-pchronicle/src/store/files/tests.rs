use super::*;
use datafusion::logical_expr::{col, lit};

fn atif_fixture(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/atif")
        .join(name)
}

#[test]
fn virtual_column_does_not_change_lance_schemas() {
    assert!(story_runs_arrow_schema()
        .field_with_name(SOURCE_FILE_COLUMN)
        .is_err());
    assert!(query_schema(&story_runs_arrow_schema())
        .field_with_name(SOURCE_FILE_COLUMN)
        .is_ok());
}

#[test]
fn file_filter_matching_supports_sql_like_and_exact_values() {
    let like = col(SOURCE_FILE_COLUMN).like(lit("batch/%_two.json"));
    assert_eq!(matches_file_filter(&like, "batch/one_two.json"), Some(true));
    assert_eq!(matches_file_filter(&like, "other/two.json"), Some(false));
    let exact = col(SOURCE_FILE_COLUMN).eq(lit("one.json"));
    assert_eq!(matches_file_filter(&exact, "one.json"), Some(true));
    assert_eq!(matches_file_filter(&exact, "two.json"), Some(false));
    assert_eq!(matches_file_filter(&col("session_id"), "one.json"), None);
}

#[test]
fn atif_step_filter_compilation_is_conservative() {
    let filter = col("session_id")
        .eq(lit("run-a"))
        .and(col("step_id").gt_eq(lit(5_i64)))
        .and(col("step_id").lt_eq(lit(15_i64)));
    let compiled = atif_step_filters(&filter).expect("supported conjunction");
    let scan = FileScanSpec {
        projection: Some(Arc::from(vec![1, 2, 6])),
        projected_names: Arc::new(
            ["session_id", "step_id", "source"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        ),
        step_filters: Arc::from(compiled),
    };
    assert!(scan.matches_document("run-a"));
    assert!(!scan.matches_document("run-b"));
    assert!(scan.matches_step(5, "agent"));
    assert!(scan.matches_step(15, "agent"));
    assert!(!scan.matches_step(4, "agent"));
    assert!(!scan.matches_step(16, "agent"));
    assert!(atif_step_filters(&col("message_json").eq(lit("x"))).is_none());
}

#[test]
fn private_provider_options_and_table_names_fail_closed() {
    let manifest =
        LocalQueryManifest::for_format(atif_fixture("dialogue_10.json"), DocumentFormat::Atif)
            .unwrap();
    let error = FileTrajectoryDataSource::from_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            batch_size: 0,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("batch_size"));

    assert!(validate_table_names(&StorylineDataFusionTableNames {
        runs: "same".into(),
        steps: "same".into(),
        tool_calls: "tools".into(),
    })
    .is_err());
    assert!(validate_table_names(&StorylineDataFusionTableNames {
        runs: "".into(),
        steps: "steps".into(),
        tool_calls: "tools".into(),
    })
    .is_err());
}

#[tokio::test]
async fn projected_ndjson_enforces_private_record_bound() {
    let trajectory = std::fs::read_to_string(atif_fixture("dialogue_10.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&trajectory).unwrap();
    let input = tempfile::NamedTempFile::with_suffix(".ndjson").unwrap();
    std::fs::write(
        input.path(),
        format!("{}\n", serde_json::to_string(&value).unwrap()),
    )
    .unwrap();
    let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
    let source = FileTrajectoryDataSource::from_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            max_record_bytes: 512,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )
    .unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();
    let error = context
        .sql("SELECT COUNT(*) FROM steps")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("max_record_bytes 512"),
        "{error:#}"
    );
}

#[tokio::test]
async fn projected_array_enforces_private_record_bound_per_element() {
    let trajectory = std::fs::read_to_string(atif_fixture("dialogue_10.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&trajectory).unwrap();
    let input = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    std::fs::write(
        input.path(),
        format!("[{}]", serde_json::to_string(&value).unwrap()),
    )
    .unwrap();
    let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
    let source = FileTrajectoryDataSource::from_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            max_record_bytes: 512,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )
    .unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();
    let error = context
        .sql("SELECT COUNT(*) FROM steps")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("max_record_bytes 512"),
        "{error:#}"
    );
}

#[tokio::test]
async fn full_atif_array_enforces_private_record_bound_per_element() {
    let trajectory = std::fs::read_to_string(atif_fixture("dialogue_10.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&trajectory).unwrap();
    let input = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    std::fs::write(
        input.path(),
        format!("[{}]", serde_json::to_string(&value).unwrap()),
    )
    .unwrap();
    let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
    let source = FileTrajectoryDataSource::from_manifest_with_options(
        manifest,
        FileTrajectoryDataSourceOptions {
            max_record_bytes: 512,
            ..FileTrajectoryDataSourceOptions::default()
        },
    )
    .unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();
    let error = context
        .sql("SELECT * FROM steps")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("max_record_bytes 512"),
        "{error:#}"
    );
}

#[tokio::test]
async fn full_atif_array_reports_bounded_input_buffer_peak() {
    let trajectory = std::fs::read_to_string(atif_fixture("dialogue_10.json")).unwrap();
    let input = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    std::fs::write(input.path(), format!("[{trajectory}]")).unwrap();
    let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
    let source = FileTrajectoryDataSource::from_manifest(manifest).unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();
    context
        .sql("SELECT * FROM steps")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();

    let peak = source.metrics().snapshot().streaming_buffer_peak_bytes;
    assert!(peak >= 64 * 1024, "peak={peak}");
    assert!(peak < 2 * 64 * 1024, "peak={peak}");
}

#[tokio::test]
async fn projected_actf_pushdown_matches_session_id_and_step_id() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/import_roundtrip/protein-assembly_trimmed.actf.json");
    let manifest = LocalQueryManifest::for_format(&path, DocumentFormat::Actf).unwrap();
    let source = FileTrajectoryDataSource::from_manifest(manifest).unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();

    let batches = context
        .sql(
            "SELECT session_id, step_id, source FROM steps WHERE session_id = 'protein-assembly-trimmed' AND step_id = 2",
        )
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();

    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
    let metrics = source.metrics().snapshot();
    assert!(metrics.rows_pruned > 0, "{metrics:?}");
}

#[tokio::test]
async fn projected_atif_pushdown_matches_session_id_not_document_id() {
    let input = tempfile::NamedTempFile::with_suffix(".json").unwrap();
    std::fs::write(
        input.path(),
        r#"{
          "schema_version":"ATIF-v1.6",
          "session_id":"session-a",
          "trajectory_id":"document-a",
          "agent":{"name":"agent","version":"1"},
          "steps":[{"step_id":2,"timestamp":"2026-01-01T00:00:00Z","source":"agent","message":{"role":"assistant","content":"ok"}}]
        }"#,
    )
    .unwrap();
    let manifest = LocalQueryManifest::for_format(input.path(), DocumentFormat::Atif).unwrap();
    let source = FileTrajectoryDataSource::from_manifest(manifest).unwrap();
    let context = SessionContext::new();
    source.register(&context).unwrap();

    let batches = context
        .sql(
            "SELECT document_id, step_id FROM steps WHERE session_id = 'session-a' AND step_id = 2",
        )
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();

    assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
}
