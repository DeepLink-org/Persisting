use super::*;
use datafusion::logical_expr::{col, lit};

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
