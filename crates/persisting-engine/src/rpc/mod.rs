//! Stable RPC boundary around search and pChronicle adapters.
//!
//! Search implementation lives in [`crate::search`]. Trajectory requests are
//! translated here and delegated to `persisting-pchronicle`; this layer owns no
//! trajectory schema or storage backend.

pub mod bridge;
pub mod dispatch;
pub mod trajectory;

#[path = "ron.rs"]
pub mod ron_api;
