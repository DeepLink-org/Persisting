//! Error types for pChronicle.

use std::path::PathBuf;

use crate::format::DocumentFormat;
use crate::{InputIssue, InputIssueKind};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidInput,
    NotFound,
    CommitConflict,
    CorruptStore,
    Unsupported,
    Io,
    Internal,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::CommitConflict => "commit_conflict",
            Self::CorruptStore => "corrupt_store",
            Self::Unsupported => "unsupported",
            Self::Io => "io",
            Self::Internal => "internal",
        }
    }
}

/// Classify legacy anyhow errors at process boundaries while storage modules
/// migrate toward typed errors.
pub fn classify_error(error: &dyn std::fmt::Display) -> ErrorCode {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("not found") || message.contains("missing session") {
        ErrorCode::NotFound
    } else if message.contains("conflict")
        || message.contains("precondition")
        || message.contains("stale")
    {
        ErrorCode::CommitConflict
    } else if message.contains("corrupt")
        || message.contains("dangling")
        || message.contains("checksum")
        || message.contains("incomplete")
    {
        ErrorCode::CorruptStore
    } else if message.contains("unsupported") || message.contains("append-only") {
        ErrorCode::Unsupported
    } else if message.contains("invalid")
        || message.contains("must ")
        || message.contains("expected ")
    {
        ErrorCode::InvalidInput
    } else if message.contains("i/o")
        || message.contains("io error")
        || message.contains("permission denied")
    {
        ErrorCode::Io
    } else {
        ErrorCode::Internal
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Input(#[from] InputIssue),

    #[error("invalid {format} document (path={path:?}, location={location:?}): {message}")]
    InvalidDocument {
        format: DocumentFormat,
        path: Option<PathBuf>,
        location: Option<String>,
        message: String,
    },

    #[error("{format} document cannot represent {stories} storylines")]
    UnsupportedCardinality {
        format: DocumentFormat,
        stories: usize,
    },

    #[error("{format} source budget exceeded (path={path:?}, budget={budget})")]
    SourceBudgetExceeded {
        format: DocumentFormat,
        path: Option<PathBuf>,
        budget: String,
    },

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

impl Error {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Input(issue) => match issue.kind() {
                InputIssueKind::Invalid => ErrorCode::InvalidInput,
                InputIssueKind::Unsupported => ErrorCode::Unsupported,
            },
            Self::InvalidDocument { .. }
            | Self::UnsupportedCardinality { .. }
            | Self::SourceBudgetExceeded { .. }
            | Self::InvalidAtif(_)
            | Self::DuplicateSession(_)
            | Self::DuplicateStep { .. }
            | Self::DuplicateToolCall { .. }
            | Self::OrphanToolCall { .. } => ErrorCode::InvalidInput,
            Self::SessionNotFound(_) => ErrorCode::NotFound,
            Self::Io(_) => ErrorCode::Io,
            Self::Json(_) => ErrorCode::InvalidInput,
            Self::Other(message) => classify_error(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Error;
    use crate::format::DocumentFormat;

    #[test]
    fn invalid_document_error_identifies_format_and_location() {
        let error = Error::InvalidDocument {
            format: DocumentFormat::OpenaiMsg,
            path: Some("sessions.json".into()),
            location: Some("record[2].messages".into()),
            message: "expected an array".into(),
        };

        let rendered = error.to_string();
        assert!(rendered.contains("openai-msg"));
        assert!(rendered.contains("sessions.json"));
        assert!(rendered.contains("record[2].messages"));
        assert_eq!(error.code(), super::ErrorCode::InvalidInput);
    }
}
