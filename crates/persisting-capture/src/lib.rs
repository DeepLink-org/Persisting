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

pub mod config;
pub mod conversion;
pub mod dead_letter;
pub mod dialogue;
pub mod dialogue_extract;
pub mod engine;
pub mod frontmatter;
pub mod injection;
pub mod lifecycle;
pub mod markdown_pipeline;
pub mod markdown_policy;
pub mod markdown_trajectory;
pub mod protocol;
pub mod provider;
pub mod proxy;
pub mod reconcile;
pub mod record;
pub mod runtime;
pub mod session;
pub mod session_client;
pub mod session_storage;
pub mod sink;
pub mod story_snapshots;
pub mod subagent_link;
pub mod trajectory_convert;
pub mod usage;

// --- short aliases (compat with existing call sites) ---

pub use runtime::debug;
pub use runtime::discover;
pub use runtime::discover as discover_daemon;
pub use runtime::in_process;
pub use runtime::run_config;
pub use runtime::run_env;
pub use runtime::service;

pub use session::chain as session_chain;
pub use session::index as session_index;

pub use proxy::models_list;

/// Used by [`sink`], [`record`], and proxy helpers (`crate::Call` in crate-internal code).
pub use engine::Call;
