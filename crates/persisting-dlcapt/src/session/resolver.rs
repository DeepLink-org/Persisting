use axum::http::HeaderMap;
use serde_json::Value;

use super::normalize::{DEFAULT_MAX_STEM_LEN, normalize_session_id};
use super::providers::{
    default_session_candidate, extract_body_metadata_session, extract_header_session,
    extract_url_path_session,
};
use super::{SessionConflict, SessionResolveError, SessionSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteSessionMode {
    Flat,
    SessionScoped,
}

pub struct RequestContext<'a> {
    pub mode: RouteSessionMode,
    pub path_session_id: Option<&'a str>,
    pub headers: &'a HeaderMap,
    pub body: &'a Value,
}

pub struct SessionIdSettings {
    pub default_session_id: String,
    pub preserve_raw: bool,
    pub session_header: String,
    pub session_header_aliases: Vec<String>,
}

pub struct ResolvedSession {
    pub storage_session_id: String,
    pub source: SessionSource,
    pub raw_value: String,
    pub conflicts: Vec<SessionConflict>,
}

pub fn resolve_session(
    ctx: &RequestContext<'_>,
    settings: &SessionIdSettings,
) -> Result<ResolvedSession, SessionResolveError> {
    match ctx.mode {
        RouteSessionMode::SessionScoped => resolve_session_scoped(ctx, settings),
        RouteSessionMode::Flat => resolve_session_flat(ctx, settings),
    }
}

fn resolve_session_scoped(
    ctx: &RequestContext<'_>,
    settings: &SessionIdSettings,
) -> Result<ResolvedSession, SessionResolveError> {
    let raw_path = ctx
        .path_session_id
        .and_then(extract_url_path_session)
        .ok_or(SessionResolveError::MissingPathSessionId)?;

    let storage_session_id =
        normalize_session_id(&raw_path, settings.preserve_raw, DEFAULT_MAX_STEM_LEN)
            .ok_or(SessionResolveError::InvalidPathSessionId)?;

    let mut conflicts = Vec::new();
    if let Some(header) = extract_header_session(
        ctx.headers,
        &settings.session_header,
        &settings.session_header_aliases,
    ) && header != raw_path
    {
        conflicts.push(SessionConflict {
            source: SessionSource::Header,
            value: header,
        });
    }
    if let Some(body) = extract_body_metadata_session(ctx.body)
        && body != raw_path
    {
        conflicts.push(SessionConflict {
            source: SessionSource::BodyMetadata,
            value: body,
        });
    }

    if !conflicts.is_empty() {
        tracing::debug!(
            storage_session_id = %storage_session_id,
            conflict_count = conflicts.len(),
            "session-scoped route ignored non-url session candidates"
        );
    }

    Ok(ResolvedSession {
        storage_session_id,
        source: SessionSource::UrlPath,
        raw_value: raw_path,
        conflicts,
    })
}

fn resolve_session_flat(
    ctx: &RequestContext<'_>,
    settings: &SessionIdSettings,
) -> Result<ResolvedSession, SessionResolveError> {
    let header = extract_header_session(
        ctx.headers,
        &settings.session_header,
        &settings.session_header_aliases,
    );
    let body = extract_body_metadata_session(ctx.body);
    let default = default_session_candidate(&settings.default_session_id);

    let (raw_value, source) = if let Some(value) = header.as_deref() {
        (value.to_string(), SessionSource::Header)
    } else if let Some(value) = body.as_deref() {
        (value.to_string(), SessionSource::BodyMetadata)
    } else if let Some(value) = default.as_deref() {
        (value.to_string(), SessionSource::Default)
    } else {
        return Err(SessionResolveError::MissingSessionId);
    };

    let storage_session_id =
        normalize_session_id(&raw_value, settings.preserve_raw, DEFAULT_MAX_STEM_LEN)
            .ok_or(SessionResolveError::InvalidSessionId)?;

    let mut conflicts = Vec::new();
    if source != SessionSource::Header
        && let Some(value) = header
    {
        conflicts.push(SessionConflict {
            source: SessionSource::Header,
            value,
        });
    }
    if source != SessionSource::BodyMetadata
        && let Some(value) = body
    {
        conflicts.push(SessionConflict {
            source: SessionSource::BodyMetadata,
            value,
        });
    }

    Ok(ResolvedSession {
        storage_session_id,
        source,
        raw_value,
        conflicts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;
    use serde_json::json;

    fn settings() -> SessionIdSettings {
        SessionIdSettings {
            default_session_id: "default".to_string(),
            preserve_raw: false,
            session_header: "x-persisting-session-id".to_string(),
            session_header_aliases: vec![],
        }
    }

    #[test]
    fn session_scoped_should_prefer_url_over_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "bbb".parse().unwrap());
        let body = json!({});
        let ctx = RequestContext {
            mode: RouteSessionMode::SessionScoped,
            path_session_id: Some("aaa"),
            headers: &headers,
            body: &body,
        };

        let resolved = resolve_session(&ctx, &settings()).expect("resolve session");
        assert_eq!(resolved.storage_session_id, "aaa");
        assert_eq!(resolved.source, SessionSource::UrlPath);
        assert_eq!(resolved.conflicts.len(), 1);
        assert_eq!(resolved.conflicts[0].value, "bbb");
    }

    #[test]
    fn flat_route_should_prefer_header_then_body_then_default() {
        let headers = HeaderMap::new();
        let body = json!({"metadata": {"session_id": "body-session"}});
        let ctx = RequestContext {
            mode: RouteSessionMode::Flat,
            path_session_id: None,
            headers: &headers,
            body: &body,
        };

        let resolved = resolve_session(&ctx, &settings()).expect("resolve session");
        assert_eq!(resolved.storage_session_id, "body-session");
        assert_eq!(resolved.source, SessionSource::BodyMetadata);
    }
}
