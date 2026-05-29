//! Embedded LLM capture proxy (agentgateway routing subset + session service).
//!
//! ## How to import
//!
//! Use **module paths**, not crate-root symbol re-exports:
//!
//! ```ignore
//! use persisting_capture::config::ProxyConfig;
//! use persisting_capture::record::CaptureRecord;
//! use persisting_capture::engine::CaptureEngine;
//! use persisting_capture::proxy::serve;
//! ```
//!
//! Short path aliases (`session_storage`, `markdown_trajectory`, …) re-export whole
//! modules only — they are not a flat public API surface.

pub mod config;
pub mod conversion;
pub mod dead_letter;
pub mod engine;
pub mod injection;
pub mod protocol;
pub mod provider;
pub mod proxy;
pub mod reconcile;
pub mod runtime;
pub mod session;
pub mod storage;
pub mod usage;

// --- module path aliases (no `pub use` of individual types at crate root) ---

pub use runtime::debug;
pub use runtime::discover;
pub use runtime::discover as discover_daemon;
pub use runtime::run_config;
pub use runtime::run_env;
pub use runtime::service;

pub use session::chain as session_chain;
pub use session::index as session_index;

pub use storage::convert as trajectory_convert;
pub use storage::dialogue;
pub use storage::dialogue_extract;
pub use storage::egress;
pub use storage::frontmatter;
pub use storage::lance_row;
pub use storage::lifecycle;
pub use storage::markdown as markdown_trajectory;
pub use storage::markdown_pipeline;
pub use storage::path_layout;
pub use storage::record;
pub use storage::session as session_storage;
pub use storage::session_client;
pub use storage::sink;
pub use storage::story_coords::{self, StoryCoords};
pub use storage::subagent_link;

pub use proxy::models_list;

/// Used by [`sink`], [`record`], and proxy helpers (`crate::Call` in crate-internal code).
pub use engine::Call;
