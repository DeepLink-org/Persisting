//! pChronicle — Persisting's structured storage layer for Agent trajectories.
//!
//! pChronicle owns the trajectory formats, physical schemas, storage backends,
//! replay, conversion, search, and rebuildable views. Capture and
//! clients call pChronicle directly; there is no separate storage engine layer.
//!
//! # Format architecture
//!
//! Storyline is the **hub** (ATIF-aligned authoritative model).
//! All other formats convert only through storyline:
//!
//! ```text
//! events ──┐
//! agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif / actf
//! openai_msg┤
//! atif ─────┤
//! actf ─────┘
//! ```
//!
//! Peripheral formats expose explicit parse/encode functions around Storyline.

mod agenticmd;
#[cfg(feature = "lance-store")]
mod append_queue;
mod atif;
mod convert;
#[cfg(feature = "lance-store")]
mod discovery;
pub mod document;
mod error;
mod format;
mod formats;
mod interop;
mod layout;
#[cfg(feature = "search")]
mod messages;
pub mod model;
#[cfg(feature = "search")]
mod operations;
#[cfg(feature = "lance-store")]
mod projection;
pub mod query;
#[cfg(feature = "lance-store")]
mod revision;
#[cfg(feature = "search")]
pub mod search;
pub mod storage;
mod store;

#[cfg(feature = "lance-store")]
pub(crate) use document::{QueryCapabilities, QueryTables};
pub(crate) use error::{Error, Result};
pub(crate) use format::DocumentFormat;
pub(crate) use formats::{
    EventIdentity, EventRecord, FieldPresence, StoryLink, StorylineAgent, StorylineDocument,
    StorylineToolCall, StorylineTurn,
};
#[cfg(feature = "search")]
pub use messages::*;
#[cfg(feature = "search")]
pub use operations::bridge::{
    search_add, search_add_batch, search_import_lance, search_index, search_index_delete,
    search_index_list, search_index_rebuild, search_index_reorder, search_query,
};
#[cfg(feature = "search")]
pub use operations::dispatch::invoke_request_body;
#[cfg(feature = "search")]
pub use search::agent as agent_search;
#[cfg(feature = "lance-store")]
pub(crate) use store::{
    event_record_to_event_row, event_row_to_event_record, event_rows_from_batch, EventRow,
    TRAJECTORY_AGENT_ID_COL, TRAJECTORY_CALL_ID_COL, TRAJECTORY_COLS, TRAJECTORY_EVENT_ID_COL,
    TRAJECTORY_KIND_COL, TRAJECTORY_MODEL_COL, TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_PAYLOAD_JSON_COL, TRAJECTORY_SEQ_COL, TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_SOURCE_COL, TRAJECTORY_TIMESTAMP_COL, TRAJECTORY_TRACE_ID_COL,
};
#[cfg(feature = "search")]
pub const PERSISTING_VECTOR_INDEX_NAME: &str = search::search_lance::PERSISTING_VECTOR_INDEX_NAME;
#[cfg(feature = "search")]
pub const PERSISTING_FTS_INDEX_NAME: &str = search::search_lance::PERSISTING_FTS_INDEX_NAME;

#[cfg(all(test, feature = "lance-store"))]
mod tests;
