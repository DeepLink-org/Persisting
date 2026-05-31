//! Trajectory storage: Vortex event log, TLV Markdown, session routing, capture sink.

pub mod convert;
pub mod dialogue;
pub mod dialogue_extract;
pub mod egress;
pub mod event_row;
pub mod frontmatter;
pub mod lifecycle;
pub mod markdown;
pub mod markdown_pipeline;
pub mod markdown_policy;
pub mod path_layout;
pub mod record;
pub mod session;
pub mod session_client;
pub mod sink;
pub mod story_coords;
pub mod story_snapshots;
pub mod subagent_link;
