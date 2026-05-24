//! Embedded LLM capture proxy (agentgateway routing subset + session service).

pub mod admin;
pub mod auth;
pub mod capture_call;
pub mod config;
pub mod conversion;
pub mod debug;
pub mod dialogue;
pub mod dialogue_extract;
pub mod discover_daemon;
pub mod forward;
pub mod http_headers;
pub mod markdown_trajectory;
pub mod models_list;
pub mod peer_process;
pub mod protocol;
pub mod provider;
pub mod proxy;
pub mod record;
pub mod router;
pub mod run_config;
pub mod run_env;
pub mod service;
pub mod session_chain;
pub mod session_client;
pub mod session_index;
pub mod session_storage;
pub mod sink;
pub mod subagent_link;
pub mod usage;

pub use config::{CaptureLevel, ModelRoute, ProxyConfig};
pub use conversion::{
    completions_response_to_messages, messages_request_to_completions,
    translate_completions_sse_to_messages, CompletionsStreamTranslator, ProtocolBridge,
};
pub use debug::{
    debug_log_path, enable_debug, is_debug_enabled, ENV_CAPTURE_DEBUG, ENV_CAPTURE_DEBUG_STDERR,
};
pub use dialogue::import_markdown_to_engine_lines;
pub use discover_daemon::{StorageResolution, StorageSource};
pub use markdown_trajectory::{
    append_engine_lines_to_markdown, is_trajectory_markdown_path, locate_session_markdown,
    session_markdown_path, session_markdown_write_path, BlockHeader, MarkdownBlock,
    LEGACY_TRAJECTORY_MARKDOWN_FILENAME, SESSION_MARKDOWN_FILENAME,
};
pub use models_list::build_models_response;
pub use proxy::{serve, serve_with_shutdown_and_ready};
pub use record::{
    engine_line_to_record, record_to_engine_line, records_to_engine_lines, CaptureRecord,
};
pub use router::{resolve_route, resolve_route_config, ResolvedRoute};
pub use run_config::{
    load_session_proxy_config, session_proxy_config_path, snapshot_run_proxy_config,
    SESSION_PROXY_FILENAME,
};
pub use run_env::{
    apply_daemon_env, load_daemon_env_snapshot, proxy_environment, snapshot_daemon_env,
    write_run_child_info, write_run_session, CAPTURE_PROXY_ENV_KEYS, DAEMON_ENV_FILENAME,
    ENV_SESSION_ID, STANDARD_DAEMON_ENV_KEYS,
};
pub use service::{resolve_storage_detailed, stop_daemon, write_current, CaptureDaemonState};
pub use session_index::{discover_sessions, SessionSummary};
pub use session_storage::{resolve_capture_route, trajectory_session_dir, CaptureRoute};
pub use sink::{CallbackSink, CaptureSink};
pub use usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
    TokenUsage,
};
