//! Extract human-readable dialogue text from LLM request/response payloads.
//!
//! Submodules by wire protocol:
//! - [`messages`] — Anthropic Messages (`/v1/messages`)
//! - [`responses`] — OpenAI Responses (`/v1/responses`)
//! - [`completions`] — OpenAI Chat Completions (`/v1/chat/completions`)

mod assistant;
mod common;
mod completions;
mod heuristics;
mod messages;
mod request;
mod responses;

pub use assistant::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
pub use completions::CompletionsSseToolParser;
pub use heuristics::{
    is_main_agent_continuation, is_main_agent_continuation_payload, is_subagent_shape_payload,
};
pub use messages::{push_sse_tool_snapshot, ContentBlock, SseStreamBlockParser};
pub use request::{count_visible_user_messages, extract_user_message_from_request_body};
