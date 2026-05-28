//! OpenAI-compatible HTTP proxy with capture on request/response.

pub mod admin;
pub mod auth;
pub mod common;
pub mod dispatch;
pub mod forward;
pub mod http_headers;
pub mod llm_capture;
pub mod model;
pub mod models_list;
pub mod reasoning;
pub mod router;
pub mod state;
pub mod streaming;
pub mod upstream;

pub use auth::{apply_upstream_headers, resolve_upstream_api_key};
pub use model::rewrite_model_in_body;
pub use reasoning::ReasoningCacheHandle;
pub use state::{serve, serve_with_shutdown, serve_with_shutdown_and_ready, ProxyState};
pub use upstream::prepare_upstream_body;
