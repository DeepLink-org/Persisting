//! Compile-time guard for pChronicle's intentional public entrypoints.

use persisting_pchronicle::Result;
use persisting_pchronicle::document::{
    DocumentFormat, InputIssue, InputIssueKind, InputResult, decode_json_storylines,
    encode_json_storylines,
};
use persisting_pchronicle::model::{EventRecord, StorylineDocument};

#[cfg(feature = "lance-store")]
use persisting_pchronicle::query::{ChronicleQueryEngine, QueryCapabilities};
#[cfg(feature = "lance-store")]
use persisting_pchronicle::search::{FindExpr, FindTextField, parse_match_expression};

#[test]
fn approved_facade_paths_compile() {
    let _: DocumentFormat = DocumentFormat::Atif;
    let _: InputResult<Vec<StorylineDocument>> =
        decode_json_storylines(DocumentFormat::Atif, "{}", "compile-only.json");
    let _: fn(DocumentFormat, &[StorylineDocument]) -> Result<serde_json::Value> =
        encode_json_storylines;
    let _: Option<EventRecord> = None;

    #[cfg(feature = "lance-store")]
    {
        let _: Option<ChronicleQueryEngine> = None;
        let _: Option<QueryCapabilities> = None;
        let _: fn(&str) -> Result<FindExpr> = parse_match_expression;
        let _: Option<FindTextField> = None;
    }
}

fn accepts_anyhow<T>(result: anyhow::Result<T>) -> anyhow::Result<T> {
    result
}

#[test]
fn result_alias_is_anyhow() {
    let result: Result<()> = Ok(());
    let _: anyhow::Result<()> = accepts_anyhow(result);
}

#[test]
fn public_errors_are_operational_or_input_local() {
    let issue = InputIssue::invalid("invalid JSON").at("turns[0]");
    assert_eq!(issue.kind(), InputIssueKind::Invalid);
    assert_eq!(issue.message(), "invalid JSON");
    assert_eq!(issue.location(), Some("turns[0]"));
    let _: InputResult<()> = Err(issue);
}

#[test]
fn public_decoder_preserves_unsupported_input_issues() {
    let actf = serde_json::json!({
        "task_id": "task",
        "category": "category",
        "k": 1,
        "correct": false,
        "attempts_tried": 1,
        "solved_at": null,
        "attempts": {
            "attempt": {
                "correct": false,
                "final_answer": null,
                "ground_truth": "",
                "trajectory": {
                    "schema_version": "ACTF_v0.9",
                    "steps": [],
                    "started_at": "start",
                    "finished_at": "end"
                },
                "status": "failed",
                "score": null,
                "error": "",
                "artifacts": null,
                "extra": null,
                "analysis_result": null,
                "meta": null
            }
        }
    });
    let actf_error =
        decode_json_storylines(DocumentFormat::Actf, &actf.to_string(), "task.json").unwrap_err();
    assert_eq!(actf_error.kind(), InputIssueKind::Unsupported);

    let openai_error = decode_json_storylines(
        DocumentFormat::OpenaiMsg,
        r#"{"session_steps":[]}"#,
        "empty.json",
    )
    .unwrap_err();
    assert_eq!(openai_error.kind(), InputIssueKind::Unsupported);
}

#[test]
fn public_openai_decode_issues_do_not_include_source_paths() {
    let error = decode_json_storylines(
        DocumentFormat::OpenaiMsg,
        r#"{"session_steps":[{}]}"#,
        "sentinel-private-path.json",
    )
    .unwrap_err();
    assert_eq!(error.location(), Some("rows[0].session_id"));
    assert!(!error.message().contains("sentinel-private-path.json"));
}
