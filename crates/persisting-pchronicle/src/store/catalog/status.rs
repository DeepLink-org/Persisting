//! Shared catalog state vocabulary for UI, CLI, and query callers.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogConsistency {
    BestEffort,
    #[serde(rename = "per_source_pinned")]
    Pinned,
    Exact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogState {
    Ready,
    Stale,
    Refreshing,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogStatus {
    pub consistency: CatalogConsistency,
    pub state: CatalogState,
    pub generation: String,
    pub observed_at: i64,
    pub last_error: Option<String>,
}
