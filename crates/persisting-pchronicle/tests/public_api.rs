//! Compile-time guard for pChronicle's four intentional public entrypoints.

use persisting_pchronicle::document::{
    decode_json_storylines, encode_json_storylines, DocumentFormat, Result as DocumentResult,
};
use persisting_pchronicle::model::{EventRecord, StorylineDocument};

#[cfg(feature = "lance-store")]
use persisting_pchronicle::query::{ChronicleQueryEngine, QueryCapabilities};

#[test]
fn approved_facade_paths_compile() {
    let _: DocumentFormat = DocumentFormat::Atif;
    let _: DocumentResult<Vec<StorylineDocument>> =
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
