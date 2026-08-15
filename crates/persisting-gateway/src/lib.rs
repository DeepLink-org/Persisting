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
#[cfg(feature = "echo-server")]
pub mod echo;
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
pub mod understanding;
pub mod usage;

pub use gateway::models_list;
pub use gateway::{
    serve, serve_with_listeners_and_shutdown, serve_with_runtime_control, serve_with_shutdown,
    serve_with_shutdown_and_ready,
};

/// Used by [`sink`], [`record`], and gateway helpers (`crate::Call` internally).
pub use engine::Call;
