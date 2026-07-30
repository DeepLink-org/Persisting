//! Error types for pChronicle.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid ATIF: {0}")]
    InvalidAtif(String),

    #[error("session not found: {0}")]
    SessionNotFound(String),

    #[error("duplicate session_id: {0}")]
    DuplicateSession(String),

    #[error("duplicate step ({session_id}, {step_id})")]
    DuplicateStep { session_id: String, step_id: i64 },

    #[error("duplicate tool_call ({session_id}, {tool_call_id})")]
    DuplicateToolCall {
        session_id: String,
        tool_call_id: String,
    },

    #[error("tool_call {tool_call_id} references missing step {step_id} in session {session_id}")]
    OrphanToolCall {
        session_id: String,
        step_id: i64,
        tool_call_id: String,
    },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}
