//! Embedded LLM capture proxy (agentgateway routing subset + session service).

pub mod config;
pub mod conversion;
pub mod dead_letter;
pub mod engine;
pub mod protocol;
pub mod provider;
pub mod proxy;
pub mod reconcile;
pub mod runtime;
pub mod session;
pub mod storage;
pub mod usage;

pub use proxy::models_list;
pub use proxy::{serve, serve_with_shutdown_and_ready};
pub use runtime::debug;
pub use runtime::discover as discover_daemon;
pub use runtime::run_config;
pub use runtime::run_env;
pub use runtime::service;
pub use session::call as capture_call;
pub use session::chain as session_chain;
pub use session::index as session_index;
pub use session::peer_process;

pub use storage::convert as trajectory_convert;
pub use storage::dialogue;
pub use storage::dialogue_extract;
pub use storage::frontmatter::{
    format_run_summary_line, refresh_run_markdown_frontmatter, SessionFrontmatterSummary,
};
pub use storage::lance_row;
pub use storage::lifecycle;
pub use storage::markdown as markdown_trajectory;
pub use storage::markdown_pipeline;
pub use storage::markdown_pipeline::{
    skip_markdown_block, LiveMarkdownWriter, MarkdownPipeline, MarkdownTarget,
};
pub use storage::record;
pub use storage::session as session_storage;
pub use storage::session_client;
pub use storage::sink;
pub use storage::subagent_link;

pub use config::{CaptureLevel, ModelRoute, ProxyConfig};
pub use conversion::{
    completions_response_to_messages, completions_response_to_responses,
    messages_request_to_completions, responses_request_to_completions,
    translate_completions_sse_to_messages, translate_completions_sse_to_responses,
    CompletionsStreamTranslator, CompletionsToResponsesStreamTranslator, ProtocolBridge,
    StreamTranslator,
};
pub use dead_letter::{
    append_dead_letter, dead_letter_path, read_dead_letter_entries, replay_dead_letter,
    DeadLetterEntry, DeadLetterInvocation, DeadLetterReplaySummary, SerializableCaptureEvent,
};
pub use debug::{
    debug_log_path, enable_debug, is_debug_enabled, ENV_CAPTURE_DEBUG, ENV_CAPTURE_DEBUG_STDERR,
};
pub use dialogue::import_markdown_to_engine_lines;
pub use discover_daemon::{StorageResolution, StorageSource};
pub use engine::{
    CaptureEngine, CaptureEvent, CaptureInvocation, LlmCallCancelled, LlmRequestCaptured,
    LlmResponseCompleted, LlmResponseDraftUpdated,
};
pub use lance_row::{
    capture_record_to_event_row, engine_line_to_event_row, event_row_to_capture_record,
    event_row_to_replay_json, LanceEventRow,
};
pub use lifecycle::{
    append_lifecycle, proxy_lifecycle_route, root_session_route, session_ended_record,
    session_started_record, session_state_record, CaptureMode, SessionLifecyclePayload,
    SESSION_ENDED, SESSION_STARTED, SESSION_STATE,
};
pub use markdown_trajectory::{
    append_engine_lines_to_markdown, is_subagent_session_storage_key, is_trajectory_markdown_path,
    locate_run_bucket_markdown, locate_session_markdown, locate_session_markdown_for_key,
    sanitize_session_filename, session_markdown_filename, session_markdown_path,
    session_markdown_path_for_key, session_markdown_write_path,
    session_markdown_write_path_for_key, upsert_block_by_call_id, BlockHeader, MarkdownBlock,
    LEGACY_TRAJECTORY_MARKDOWN_FILENAME, SESSION_MARKDOWN_FILENAME,
};
pub use models_list::build_models_response;
pub use reconcile::{
    build_run_report, expected_markdown_call_ids, index_markdown_path, list_run_markdown_paths,
    reconcile_session, write_run_reconcile_report, RunReconcileReport, SessionReconcile,
};
pub use record::{
    engine_line_to_record, record_to_engine_line, records_to_engine_lines, CaptureRecord,
};
pub use router::{resolve_route, resolve_route_config, ResolvedRoute};
pub use run_config::{
    load_session_proxy_config, session_proxy_config_path, snapshot_run_proxy_config,
    SESSION_PROXY_FILENAME,
};
pub use run_env::{
    apply_daemon_env, capture_openai_v1_base, client_gateway_config_args, daily_run_id,
    ensure_serve_run_session, load_daemon_env_snapshot, proxy_environment, snapshot_daemon_env,
    write_run_child_info, write_run_session, CAPTURE_PROXY_ENV_KEYS, DAEMON_ENV_FILENAME,
    ENV_SESSION_ID, STANDARD_DAEMON_ENV_KEYS,
};
pub use service::{resolve_storage_detailed, stop_daemon, write_current, CaptureDaemonState};
pub use session_index::{discover_sessions, SessionSummary};
pub use session_storage::{
    resolve_capture_route, trajectory_markdown_path, trajectory_run_dir, trajectory_session_dir,
    CaptureRoute,
};
pub use sink::{CallbackSink, CaptureSink};
pub use trajectory_convert::{
    capture_records_to_markdown_blocks, compact_stats_note, markdown_document_to_capture_records,
    markdown_document_to_engine_lines, materialize_markdown_path, materialize_records_to_markdown,
    stream_engine_lines_to_markdown, write_markdown_document, CompactStats, MaterializeStats,
    StreamMaterializeStats,
};
pub use usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
    TokenUsage,
};

pub use proxy::router;
