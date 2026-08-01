//! Agent protocol gateway: forwarding, trajectory capture, and live projection.
//!
//! ## How to import
//!
//! Use the crate root for serving and modules for domain types:
//!
//! ```ignore
//! use persisting_gateway::config::ProxyConfig;
//! use persisting_gateway::record::CaptureRecord;
//! use persisting_gateway::engine::CaptureEngine;
//! use persisting_gateway::serve;
//! ```

pub mod config;
pub mod conversion;
pub mod dead_letter;
pub mod dialogue_extract;
pub mod engine;
mod gateway;
pub mod injection;
pub mod lifecycle;
pub mod projection;
pub mod protocol;
pub mod provider;
pub mod record;
pub mod runtime;
pub mod session;
pub mod sink;
pub mod subagent_link;
pub mod usage;

// Compatibility paths for callers migrating to the grouped projection/session layout.
pub use projection::{
    dialogue, frontmatter, markdown_pipeline, markdown_policy, markdown_trajectory, reconcile,
};
pub use session::{
    client as session_client, snapshots as story_snapshots, storage as session_storage,
};

// --- runtime/session short aliases (compat with existing call sites) ---

pub use runtime::debug;
pub use runtime::discover;
pub use runtime::discover as discover_daemon;
pub use runtime::in_process;
pub use runtime::run_config;
pub use runtime::run_env;
pub use runtime::service;

pub use session::chain as session_chain;
pub use session::index as session_index;

pub use gateway::models_list;
pub use gateway::{
    serve, serve_with_runtime_control, serve_with_shutdown, serve_with_shutdown_and_ready,
};

/// Used by [`sink`], [`record`], and gateway helpers (`crate::Call` internally).
pub use engine::Call;
