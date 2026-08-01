//! LLM protocol adaptation and trajectory capture on top of overlaynet.

mod admin;
mod auth;
mod common;
mod dispatch;
mod llm_capture;
mod model;
pub mod models_list;
mod overlaynet_config;
mod reasoning;
mod router;
mod state;
mod streaming;
mod upstream;

pub(crate) use reasoning::ReasoningCacheHandle;
pub use state::{
    serve, serve_with_runtime_control, serve_with_shutdown, serve_with_shutdown_and_ready,
};
