use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::FromRequestParts;
use axum::http::Request;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use serde_json::Value;

use super::problem::{
    FourXxRootCause, LOG_TARGET, QUERY_LOG_LIMIT, new_request_id, parse_incoming_request_id,
    truncate_utf8,
};

#[derive(Clone, Debug)]
pub(crate) struct RequestId(pub String);

impl RequestId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Default)]
pub(crate) struct FtsDiagnostics(pub Arc<Mutex<Vec<String>>>);

impl FtsDiagnostics {
    pub(crate) fn push(&self, message: impl Into<String>) {
        if let Ok(mut errors) = self.0.lock() {
            errors.push(message.into());
        }
    }

    pub(crate) fn extend(&self, messages: impl IntoIterator<Item = String>) {
        if let Ok(mut errors) = self.0.lock() {
            errors.extend(messages);
        }
    }

    fn joined(&self) -> String {
        self.0
            .lock()
            .map(|errors| truncate_utf8(&errors.join("; "), QUERY_LOG_LIMIT))
            .unwrap_or_default()
    }
}

impl<S> FromRequestParts<S> for RequestId
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .unwrap_or_else(|| RequestId(String::new())))
    }
}

impl<S> FromRequestParts<S> for FtsDiagnostics
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<FtsDiagnostics>()
            .cloned()
            .unwrap_or_default())
    }
}

pub(crate) async fn warehouse_request_layer(
    mut request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let incoming = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_incoming_request_id);
    let request_id = incoming.unwrap_or_else(new_request_id);
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let started = Instant::now();
    let fts = FtsDiagnostics::default();
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    request.extensions_mut().insert(fts.clone());

    let response = next.run(request).await;
    let status = response.status();
    let root_cause = response
        .extensions()
        .get::<FourXxRootCause>()
        .map(|value| value.0.clone())
        .unwrap_or_default();
    let (response, error_fields) = attach_request_id(response, &request_id).await;

    let is_api = path.starts_with("/api/");
    if is_api {
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            target: LOG_TARGET,
            request_id = %request_id,
            method = %method,
            path = %path,
            status = status.as_u16(),
            elapsed_ms,
            query = %truncate_utf8(&query, QUERY_LOG_LIMIT),
            "warehouse request"
        );
        if (400..500).contains(&status.as_u16()) {
            let (code, message) = error_fields.unwrap_or_default();
            let fts_errors = fts.joined();
            tracing::warn!(
                target: LOG_TARGET,
                request_id = %request_id,
                code = %code,
                message = %message,
                root_cause = %root_cause,
                fts_errors = %fts_errors,
                "warehouse request rejected"
            );
        }
    }
    response
}

async fn attach_request_id(
    response: Response,
    request_id: &str,
) -> (Response, Option<(String, String)>) {
    let (mut parts, body) = response.into_parts();
    if let Ok(value) = HeaderValue::from_str(request_id) {
        parts.headers.insert("x-request-id", value);
    }
    let is_json = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("json"));
    if is_json && parts.status.as_u16() >= 400 {
        match axum::body::to_bytes(body, 1024 * 1024).await {
            Ok(bytes) => {
                let (bytes, fields) = inject_request_id_json(bytes.to_vec(), request_id);
                return (
                    Response::from_parts(parts, axum::body::Body::from(bytes)),
                    fields,
                );
            }
            Err(_) => {
                return (Response::from_parts(parts, axum::body::Body::empty()), None);
            }
        }
    }
    (Response::from_parts(parts, body), None)
}

fn inject_request_id_json(bytes: Vec<u8>, request_id: &str) -> (Vec<u8>, Option<(String, String)>) {
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return (bytes, None);
    };
    let fields = value.as_object().map(|object| {
        (
            object
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("request_id".into(), Value::String(request_id.to_owned()));
        if let Ok(encoded) = serde_json::to_vec(&value) {
            return (encoded, fields);
        }
    }
    (bytes, fields)
}

pub(crate) fn tracing_filter(level: crate::LogLevel) -> String {
    let level = match level {
        crate::LogLevel::Error => "error",
        crate::LogLevel::Warn => "warn",
        crate::LogLevel::Info => "info",
        crate::LogLevel::Debug => "debug",
    };
    format!("pchronicle.serve={level}")
}

pub(crate) fn init_warehouse_tracing(level: crate::LogLevel) {
    // Synchronous stderr is enough: lines are short. Do not wrap this in an
    // async logger while `main` holds `stdout.lock()` — on macOS `Stderr`
    // writes take that same lock and deadlock Tokio workers.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(tracing_filter(level)))
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

pub(crate) fn log_warehouse_startup(listen: &str, datasets: &[String], snapshot_id: Option<&str>) {
    let datasets = datasets.join(",");
    if let Some(snapshot_id) = snapshot_id {
        tracing::info!(
            target: LOG_TARGET,
            listen = %listen,
            datasets = %datasets,
            snapshot_id = %snapshot_id,
            "warehouse listening"
        );
    } else {
        tracing::info!(
            target: LOG_TARGET,
            listen = %listen,
            datasets = %datasets,
            "warehouse listening"
        );
    }
}
