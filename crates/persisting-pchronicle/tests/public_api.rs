//! Compile-time guard for pChronicle's four intentional public entrypoints.

use persisting_pchronicle::document::{
    atif_to_storyline, parse_agenticmd, DocumentFormat, Result as DocumentResult,
};
use persisting_pchronicle::model::{AtifTrajectory, EventRecord, StorylineDocument};
use persisting_pchronicle::storage::{
    reconstruct_storyline, split_storyline, Result as StorageResult, StorylineTables,
};

#[cfg(feature = "lance-store")]
use persisting_pchronicle::query::{ChronicleQueryEngine, QueryCapabilities};

#[test]
fn approved_facade_paths_compile() {
    let _: DocumentFormat = DocumentFormat::Atif;
    let _: fn(&AtifTrajectory) -> DocumentResult<StorylineDocument> = atif_to_storyline;
    let _: fn(&str) -> DocumentResult<StorylineDocument> = parse_agenticmd;
    let _: fn(&StorylineDocument) -> StorageResult<StorylineTables> = split_storyline;
    let _: fn(StorylineTables) -> StorageResult<StorylineDocument> = reconstruct_storyline;
    let _: Option<EventRecord> = None;

    #[cfg(feature = "lance-store")]
    {
        let _: Option<ChronicleQueryEngine> = None;
        let _: Option<QueryCapabilities> = None;
    }
}
