mod normalize;
mod providers;
mod resolver;

pub use normalize::{DEFAULT_MAX_STEM_LEN, normalize_session_id};
pub use resolver::{
    RequestContext, ResolvedSession, RouteSessionMode, SessionIdSettings, resolve_session,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSource {
    UrlPath,
    Header,
    BodyMetadata,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConflict {
    pub source: SessionSource,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionResolveError {
    MissingPathSessionId,
    InvalidPathSessionId,
    MissingSessionId,
    InvalidSessionId,
}

// Backward-compatible helpers for existing tests.
pub use providers::extract_header_session as extract_session_from_headers;

pub fn session_id_from_body(body: &serde_json::Value) -> Option<String> {
    providers::extract_body_metadata_session(body)
}

pub fn resolve_session_id(
    header_session_id: Option<&str>,
    body: &serde_json::Value,
    default: &str,
) -> String {
    resolve_session_with_source(header_session_id, body, default).0
}

pub fn resolve_session_with_source(
    header_session_id: Option<&str>,
    body: &serde_json::Value,
    default: &str,
) -> (String, SessionSource) {
    let mut headers = axum::http::HeaderMap::new();
    if let Some(value) = header_session_id.filter(|value| !value.trim().is_empty())
        && let Ok(header_value) = value.parse()
    {
        headers.insert("x-session-id", header_value);
    }

    let settings = SessionIdSettings {
        default_session_id: default.to_string(),
        preserve_raw: false,
        session_header: "x-persisting-session-id".to_string(),
        session_header_aliases: vec![],
    };
    let ctx = RequestContext {
        mode: RouteSessionMode::Flat,
        path_session_id: None,
        headers: &headers,
        body,
    };

    match resolve_session(&ctx, &settings) {
        Ok(resolved) => (resolved.storage_session_id, resolved.source),
        Err(_) => (default.to_string(), SessionSource::Default),
    }
}
