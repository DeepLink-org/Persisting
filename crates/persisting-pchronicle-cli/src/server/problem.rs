use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use persisting_pchronicle::document::{InputIssue, InputIssueKind};
use serde::Serialize;

pub(crate) const LOG_TARGET: &str = "pchronicle.serve";
pub(crate) const QUERY_LOG_LIMIT: usize = 512;
pub(crate) const ROOT_CAUSE_LIMIT: usize = 512;
pub(crate) const CHAIN_LIMIT: usize = 2048;

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

pub(crate) fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let end = input
        .char_indices()
        .map(|(i, ch)| i + ch.len_utf8())
        .take_while(|end| *end <= max_bytes)
        .last()
        .unwrap_or(0);
    format!("{}…", &input[..end])
}

pub(crate) fn new_request_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

pub(crate) fn parse_incoming_request_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || value.bytes().any(|byte| byte <= 0x20 || byte >= 0x7f)
    {
        return None;
    }
    Some(value.to_owned())
}

#[derive(Clone, Debug)]
pub(crate) struct FourXxRootCause(pub String);

#[derive(Debug, Serialize)]
pub(super) struct ApiError {
    #[serde(skip)]
    pub(super) status: StatusCode,
    pub(super) code: BoundaryCode,
    message: String,
    request_id: String,
    #[serde(skip)]
    root_cause: Option<String>,
}

impl ApiError {
    fn public(status: StatusCode, code: BoundaryCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            request_id: String::new(),
            root_cause: None,
        }
    }

    pub(super) fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = request_id.into();
        self
    }

    pub(super) fn with_4xx_root_cause(self, error: &anyhow::Error) -> Self {
        if error.source().is_some() {
            self.with_4xx_root_cause_text(truncate_utf8(
                &error.root_cause().to_string(),
                ROOT_CAUSE_LIMIT,
            ))
        } else {
            self
        }
    }

    fn with_4xx_root_cause_text(mut self, root_cause: String) -> Self {
        if (400..500).contains(&self.status.as_u16()) {
            self.root_cause = Some(root_cause);
        }
        self
    }

    pub(super) fn from_anyhow(
        request_id: impl AsRef<str>,
        handler: &'static str,
        error: anyhow::Error,
    ) -> Self {
        let request_id = request_id.as_ref();
        let deeper = error.source().is_some();
        let root_cause = truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT);
        let api = if let Some(boundary) = error.downcast_ref::<CliBoundaryError>() {
            Self::from_boundary(request_id, boundary.code, boundary.message.clone())
        } else {
            let message = format!("{error:#}");
            if message.contains("FTS unavailable") {
                Self::invalid_request(message).with_request_id(request_id)
            } else {
                return Self::internal(request_id, handler, error);
            }
        };
        if deeper {
            api.with_4xx_root_cause_text(root_cause)
        } else {
            api
        }
    }

    fn from_boundary(request_id: &str, code: BoundaryCode, message: String) -> Self {
        match code {
            BoundaryCode::InvalidRequest => {
                Self::invalid_request(message).with_request_id(request_id)
            }
            BoundaryCode::NotFound => Self::not_found(message).with_request_id(request_id),
            BoundaryCode::Conflict => Self::conflict(message).with_request_id(request_id),
            BoundaryCode::Unsupported => Self::unsupported(message).with_request_id(request_id),
            BoundaryCode::ResourceExhausted => {
                Self::resource_exhausted(message).with_request_id(request_id)
            }
            BoundaryCode::Unavailable => Self::unavailable().with_request_id(request_id),
            BoundaryCode::Internal => {
                Self::internal(request_id, "boundary", anyhow::anyhow!(message))
            }
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

    pub(super) fn internal(
        request_id: impl Into<String>,
        handler: &'static str,
        error: anyhow::Error,
    ) -> Self {
        let request_id = request_id.into();
        let root_cause = truncate_utf8(&error.root_cause().to_string(), ROOT_CAUSE_LIMIT);
        let chain = truncate_utf8(&format!("{error:#}"), CHAIN_LIMIT);
        tracing::error!(
            target: LOG_TARGET,
            request_id = %request_id,
            code = "internal",
            handler = %handler,
            root_cause = %root_cause,
            chain = %chain,
            "warehouse request failed"
        );
        Self::public(
            StatusCode::INTERNAL_SERVER_ERROR,
            BoundaryCode::Internal,
            "internal server error",
        )
        .with_request_id(request_id)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let root_cause = self.root_cause.clone();
        let mut response = (self.status, Json(self)).into_response();
        if let Some(root_cause) = root_cause {
            response.extensions_mut().insert(FourXxRootCause(root_cause));
        }
        response
    }
}

#[derive(Debug)]
pub(crate) struct CliBoundaryError {
    pub(crate) code: BoundaryCode,
    pub(crate) message: String,
}

impl std::fmt::Display for CliBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CliBoundaryError {}
