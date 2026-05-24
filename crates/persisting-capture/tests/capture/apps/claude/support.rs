use std::path::{Path, PathBuf};

use axum::http::header::HeaderName;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use bytes::Bytes;
use persisting_capture::session_storage::{
    resolve_capture_route, trajectory_markdown_path, trajectory_session_dir, CaptureRoute,
};

pub const RUN_ROOT: &str = "run-20260524-test";
pub const PROXY_AGENT: &str = "deepseek-proxy";
pub const CLAUDE_SESSION: &str = "5e27e4a7-f42a-42a9-8448-79608bd95c53";
pub const SESSION_HEADER: &str = "x-persisting-session-id";

/// Temp storage root with `.capture/run_session` (mimics `capture run`).
pub fn claude_run_storage() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".capture")).expect("mkdir .capture");
    std::fs::write(dir.path().join(".capture/run_session"), RUN_ROOT).expect("write run_session");
    dir
}

pub fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            HeaderName::from_bytes(k.as_bytes()).expect("header name"),
            HeaderValue::try_from(*v).expect("header value"),
        );
    }
    h
}

pub fn body_bytes(json: &str) -> Bytes {
    Bytes::copy_from_slice(json.as_bytes())
}

pub fn fixture_body(name: &str) -> Bytes {
    let path = format!(
        "{}/tests/capture/apps/claude/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"));
    Bytes::from(data)
}

pub fn resolve_claude_route(
    storage: &Path,
    header_pairs: &[(&str, &str)],
    body: &Bytes,
) -> CaptureRoute {
    resolve_capture_route(&headers(header_pairs), body, SESSION_HEADER, storage)
}

pub fn main_trajectory_dir(storage: &Path, route: &CaptureRoute) -> PathBuf {
    trajectory_session_dir(storage, PROXY_AGENT, route)
}

pub fn subagent_markdown_path(storage: &Path, route: &CaptureRoute) -> PathBuf {
    trajectory_markdown_path(storage, PROXY_AGENT, route)
}
