//! Compile-time guard for pChronicle's four intentional public entrypoints.

use persisting_pchronicle::document::{
    decode_json_storylines, encode_json_storylines, DocumentFormat, InputIssue, InputIssueKind,
    InputResult, Result as DocumentResult,
};
use persisting_pchronicle::model::{EventRecord, StorylineDocument};

#[cfg(feature = "lance-store")]
use persisting_pchronicle::query::{ChronicleQueryEngine, QueryCapabilities};

#[test]
fn approved_facade_paths_compile() {
    let _: DocumentFormat = DocumentFormat::Atif;
    let _: InputResult<Vec<StorylineDocument>> =
        decode_json_storylines(DocumentFormat::Atif, "{}", "compile-only.json");
    let _: fn(DocumentFormat, &[StorylineDocument]) -> DocumentResult<serde_json::Value> =
        encode_json_storylines;
    let _: Option<EventRecord> = None;

    #[cfg(feature = "lance-store")]
    {
        let _: Option<ChronicleQueryEngine> = None;
        let _: Option<QueryCapabilities> = None;
    }
}

#[test]
fn public_errors_are_operational_or_input_local() {
    let issue = InputIssue::invalid("invalid JSON").at("turns[0]");
    assert_eq!(issue.kind(), InputIssueKind::Invalid);
    assert_eq!(issue.message(), "invalid JSON");
    assert_eq!(issue.location(), Some("turns[0]"));
    let _: InputResult<()> = Err(issue);
}
