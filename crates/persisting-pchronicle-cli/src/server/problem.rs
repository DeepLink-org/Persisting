use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use persisting_pchronicle::document::{InputIssue, InputIssueKind};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BoundaryCode {
    InvalidRequest,
    NotFound,
    Conflict,
    Unsupported,
    ResourceExhausted,
    Unavailable,
    Internal,
}

impl BoundaryCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    #[serde(skip)]
    pub(super) status: StatusCode,
    pub(super) code: BoundaryCode,
    message: String,
}

impl ApiError {
    fn public(status: StatusCode, code: BoundaryCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub(super) fn invalid_request(message: impl Into<String>) -> Self {
        Self::public(
            StatusCode::BAD_REQUEST,
            BoundaryCode::InvalidRequest,
            message,
        )
    }

    pub(super) fn not_found(message: impl Into<String>) -> Self {
        Self::public(StatusCode::NOT_FOUND, BoundaryCode::NotFound, message)
    }

    pub(super) fn conflict(message: impl Into<String>) -> Self {
        Self::public(StatusCode::CONFLICT, BoundaryCode::Conflict, message)
    }

    pub(super) fn unsupported(message: impl Into<String>) -> Self {
        Self::public(
            StatusCode::UNPROCESSABLE_ENTITY,
            BoundaryCode::Unsupported,
            message,
        )
    }

    pub(super) fn resource_exhausted(message: impl Into<String>) -> Self {
        Self::public(
            StatusCode::TOO_MANY_REQUESTS,
            BoundaryCode::ResourceExhausted,
            message,
        )
    }

    #[allow(dead_code)]
    pub(super) fn unavailable() -> Self {
        Self::public(
            StatusCode::SERVICE_UNAVAILABLE,
            BoundaryCode::Unavailable,
            "service unavailable",
        )
    }

    pub(super) fn input(issue: InputIssue) -> Self {
        let message = issue.message().to_owned();
        match issue.kind() {
            InputIssueKind::Invalid => Self::invalid_request(message),
            InputIssueKind::Unsupported => Self::unsupported(message),
        }
    }

    pub(super) fn internal(error: anyhow::Error) -> Self {
        tracing::error!(error = ?error, "pChronicle request failed");
        Self::public(
            StatusCode::INTERNAL_SERVER_ERROR,
            BoundaryCode::Internal,
            "internal server error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}
