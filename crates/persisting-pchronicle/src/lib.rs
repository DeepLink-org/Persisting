//! pChronicle — Canonical Run History Store.
//!
//! # Format architecture
//!
//! [`ChronicleFormat::Storyline`] is the **hub** (ATIF-aligned interchange).
//! All other formats convert only through storyline:
//!
//! ```text
//! events ──┐
//! agenticmd ┼──► storyline ──► events / agenticmd / openai_msg / atif
//! openai_msg┤
//! atif ─────┘
//! ```
//!
//! Use [`convert::into_storyline`] / [`convert::from_storyline`] / [`convert::convert`].

pub mod atif;
pub mod convert;
pub mod error;
pub mod format;
pub mod formats;
pub mod ingest;
pub mod schema;
pub mod store;
pub mod view;

pub use atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
pub use convert::{convert, from_storyline, into_storyline};
pub use error::{Error, Result};
pub use format::ChronicleFormat;
pub use formats::{
    detect_format, encode_agenticmd_document, events_lance_only_message, export_events_json_pretty,
    export_events_jsonl, parse_agenticmd_document, parse_openai_msg_document,
    parse_storyline_document, AgenticmdBlock, AgenticmdDocument, AgenticmdHeader, EventRecord,
    EventsDocument, OpenaiMsgDocument, OpenaiMsgStep, StoryLink, StorylineAgent, StorylineDocument,
    StorylineToolCall, StorylineTurn, OPENAI_MSG_FORMAT_VERSION, STORYLINE_SCHEMA_VERSION,
};
pub use ingest::{ingest_trajectory, reconstruct_trajectory, split_trajectory, SplitTables};
pub use schema::{SessionRow, StepRow, ToolCallRow};
pub use store::{ChronicleStore, FsChronicleStore, MemoryChronicleStore};
pub use view::{atif_trajectory_sql_ddl, AtifTrajectoryView, AtifViewRow, ATIF_TRAJECTORY_VIEW};

#[cfg(test)]
mod tests;
