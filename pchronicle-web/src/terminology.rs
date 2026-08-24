//! User-facing product terminology.
//!
//! Keep storage and API names such as `Storyline`, `CanonicalEvent`, and
//! `CatalogEventProvenance` in their owning modules. The UI uses the simpler
//! vocabulary below so implementation details do not leak into navigation and
//! everyday workflows.

pub const DATASETS: &str = "Datasets";
pub const RUNS: &str = "Runs";
pub const ANALYSIS: &str = "Analysis";
pub const STORAGE: &str = "Storage";
pub const ASSISTANT: &str = "Assistant";
pub const TIMELINE: &str = "Timeline";
pub const STEPS: &str = "Steps";
pub const RECORDED_EVENTS: &str = "Recorded events";
pub const RECONSTRUCTED_EVENTS: &str = "Reconstructed events";
