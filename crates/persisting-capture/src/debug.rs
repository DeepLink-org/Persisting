//! Debug logging for capture proxy: all HTTP dispatch + captured LLM request/response.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::ProxyConfig;

pub const ENV_CAPTURE_DEBUG: &str = "PERSISTING_CAPTURE_DEBUG";
/// Mirror `[capture-debug]` lines to stderr (default: file only).
pub const ENV_CAPTURE_DEBUG_STDERR: &str = "PERSISTING_CAPTURE_DEBUG_STDERR";

const MAX_BODY_CHARS: usize = 8192;

pub fn debug_flag_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("debug.enabled")
}

pub fn debug_log_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("debug.log")
}

/// Enable debug until flag file is removed (`capture run --debug` / CLI helper).
pub fn enable_debug(storage: &Path) -> anyhow::Result<()> {
    let path = debug_flag_path(storage);
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(&path, "1\n")?;
    Ok(())
}

pub fn is_debug_enabled(cfg: &ProxyConfig, storage: &Path) -> bool {
    cfg.debug || env_truthy(ENV_CAPTURE_DEBUG) || debug_flag_path(storage).is_file()
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn mirror_debug_to_stderr() -> bool {
    env_truthy(ENV_CAPTURE_DEBUG_STDERR)
}

fn emit(storage: &Path, line: &str) {
    let stamped = format!("{} {line}", chrono::Utc::now().to_rfc3339());
    if mirror_debug_to_stderr() {
        eprintln!("[capture-debug] {line}");
    }
    let log_path = debug_log_path(storage);
    if let Some(dir) = log_path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(f, "{stamped}");
    }
}

pub fn truncate_body_bytes(raw: &[u8]) -> String {
    if raw.len() <= MAX_BODY_CHARS {
        return String::from_utf8_lossy(raw).into_owned();
    }
    format!(
        "{}…[truncated]",
        String::from_utf8_lossy(&raw[..MAX_BODY_CHARS])
    )
}

pub fn truncate_body(raw: &str) -> String {
    if raw.chars().count() <= MAX_BODY_CHARS {
        return raw.to_string();
    }
    let mut n = 0;
    let mut end = 0;
    for (i, c) in raw.char_indices() {
        if n >= MAX_BODY_CHARS {
            break;
        }
        end = i + c.len_utf8();
        n += 1;
    }
    format!("{}…[truncated]", &raw[..end])
}

pub fn log_daemon_env_applied(storage: &Path, keys: &[String]) {
    emit(
        storage,
        &format!("capture daemon.env.applied keys={}", keys.join(",")),
    );
}

pub fn log_daemon_start(storage: &Path, listen: &str, version: &str) {
    emit(
        storage,
        &format!("capture daemon.start listen={listen} version={version}"),
    );
}

pub fn log_connect(storage: &Path, target: &str, session_id: &str) {
    emit(
        storage,
        &format!("CONNECT target={target} session={session_id}"),
    );
}

pub fn log_forward(
    storage: &Path,
    method: &str,
    url: &str,
    session_id: &str,
    status: u16,
    body: &str,
) {
    emit(
        storage,
        &format!(
            "forward {method} {url} session={session_id} status={status} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_dispatch(storage: &Path, method: &str, uri: &str, session_id: &str, mode: &str) {
    emit(
        storage,
        &format!("dispatch {method} {uri} session={session_id} mode={mode}"),
    );
}

pub fn log_llm_request(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    protocol: &str,
    path: &str,
    upstream: &str,
    body: &str,
) {
    emit(
        storage,
        &format!(
            "capture llm.request session={session_id} agent={agent_id} model={model} \
             protocol={protocol} path={path} upstream={upstream} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_llm_auth_resolved(storage: &Path, session_id: &str, source: &str) {
    emit(
        storage,
        &format!("capture llm.auth session={session_id} source={source}"),
    );
}

pub fn log_llm_upstream_sending(storage: &Path, session_id: &str, upstream: &str) {
    emit(
        storage,
        &format!("capture llm.upstream.sending session={session_id} upstream={upstream}"),
    );
}

pub fn log_llm_upstream_headers(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    upstream: &str,
    status: u16,
    content_type: &str,
    stream_request: bool,
) {
    emit(
        storage,
        &format!(
            "capture llm.upstream.headers session={session_id} agent={agent_id} model={model} \
             upstream={upstream} status={status} content_type={content_type} stream_request={stream_request}"
        ),
    );
}

pub fn log_llm_stream_start(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    status: u16,
) {
    emit(
        storage,
        &format!(
            "capture llm.response.start session={session_id} agent={agent_id} model={model} \
             status={status} streaming=true"
        ),
    );
}

pub fn log_llm_response(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    status: u16,
    total_tokens: u64,
    body: &str,
) {
    emit(
        storage,
        &format!(
            "capture llm.response session={session_id} agent={agent_id} model={model} \
             status={status} total_tokens={total_tokens} body={}",
            truncate_body(body)
        ),
    );
}

pub fn log_llm_upstream_error(
    storage: &Path,
    session_id: &str,
    agent_id: &str,
    model: &str,
    upstream: &str,
    error: &str,
) {
    emit(
        storage,
        &format!(
            "capture llm.error session={session_id} agent={agent_id} model={model} \
             upstream={upstream} error={error}"
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_long_body() {
        let s = "x".repeat(9000);
        let t = truncate_body(&s);
        assert!(t.contains("truncated"));
        assert!(t.len() < 9000);
    }
}
