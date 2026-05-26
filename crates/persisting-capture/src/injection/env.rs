//! Child-process proxy / SDK base URL injection for `capture run`.

use std::collections::HashMap;
use std::path::Path;

pub use crate::runtime::run_env::ENV_SESSION_ID;

/// Env vars that route HTTP clients through the capture proxy (must not be set on the daemon).
pub const CAPTURE_PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// OpenAI-compatible gateway base (`http://127.0.0.1:PORT/v1`) for child LLM clients.
pub fn capture_openai_v1_base(listen: &str) -> String {
    let base = if listen.starts_with("http://") || listen.starts_with("https://") {
        listen.to_string()
    } else {
        format!("http://{listen}")
    };
    format!("{}/v1", base.trim_end_matches('/'))
}

/// Extra CLI flags for clients that ignore `OPENAI_BASE_URL` and need explicit config overrides.
///
/// Codex reads `openai_base_url` from `config.toml` (via `-c`), not from `OPENAI_BASE_URL`.
pub fn client_gateway_config_args(program: &str, listen: &str) -> Vec<String> {
    let openai_v1 = capture_openai_v1_base(listen);
    let name = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    match name {
        "codex" => vec!["-c".to_string(), format!("openai_base_url=\"{openai_v1}\"")],
        _ => Vec::new(),
    }
}

/// Build env map for subprocess: HTTP(S) forward proxy + LLM SDK base URLs.
///
/// Child processes that honor `HTTP_PROXY` / `HTTPS_PROXY` send **all** HTTP(S) traffic
/// to `listen`. The capture server:
/// - `CONNECT` → TCP tunnel (HTTPS and other TLS)
/// - absolute-URI HTTP → transparent forward, except LLM API paths (captured + yaml upstream)
/// - relative paths on `listen` (via `OPENAI_BASE_URL`) → LLM gateway + capture
pub fn proxy_environment(listen: &str, session_id: &str) -> HashMap<String, String> {
    let base = if listen.starts_with("http://") || listen.starts_with("https://") {
        listen.to_string()
    } else {
        format!("http://{listen}")
    };
    let base = base.trim_end_matches('/').to_string();
    let openai_v1 = capture_openai_v1_base(listen);

    let mut env = HashMap::new();
    for key in CAPTURE_PROXY_ENV_KEYS {
        env.insert(key.to_string(), base.clone());
    }
    // Loopback gateway requests must not be CONNECT-tunneled via HTTPS_PROXY.
    env.insert("NO_PROXY".to_string(), "127.0.0.1,localhost".to_string());
    env.insert("no_proxy".to_string(), "127.0.0.1,localhost".to_string());
    for key in [
        "OPENAI_BASE_URL",
        "OPENAI_API_BASE",
        "AZURE_OPENAI_ENDPOINT",
    ] {
        env.insert(key.to_string(), openai_v1.clone());
    }
    env.insert("ANTHROPIC_BASE_URL".to_string(), base.clone());
    env.insert("GEMINI_API_BASE".to_string(), format!("{base}/v1beta"));
    env.insert(ENV_SESSION_ID.to_string(), session_id.to_string());

    // Claude Code + DeepSeek Anthropic surface reads ANTHROPIC_AUTH_TOKEN.
    if let Ok(key) = std::env::var("DEEPSEEK_API_KEY") {
        env.insert("ANTHROPIC_AUTH_TOKEN".to_string(), key.clone());
        env.insert("ANTHROPIC_API_KEY".to_string(), key);
    }

    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_env_keys() {
        let env = proxy_environment("127.0.0.1:8080", "sess-1");
        assert_eq!(
            env.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(
            env.get("OPENAI_BASE_URL").map(String::as_str),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            env.get("NO_PROXY").map(String::as_str),
            Some("127.0.0.1,localhost")
        );
        assert_eq!(env.get(ENV_SESSION_ID).map(String::as_str), Some("sess-1"));
    }

    #[test]
    fn codex_gateway_config_args() {
        let args = client_gateway_config_args("codex", "127.0.0.1:19081");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "openai_base_url=\"http://127.0.0.1:19081/v1\"");
    }
}
