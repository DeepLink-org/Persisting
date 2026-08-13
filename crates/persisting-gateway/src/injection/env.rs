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
    proxy_environment_with_local_auth(listen, session_id, false)
}

/// Build the proxy environment and, when the trusted Gateway owns upstream
/// credentials, provide non-secret Run-scoped placeholders required by SDKs.
pub fn proxy_environment_with_local_auth(
    listen: &str,
    session_id: &str,
    local_auth: bool,
) -> HashMap<String, String> {
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

    if local_auth {
        let placeholder = format!("persisting-local-{session_id}");
        for key in [
            "OPENAI_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
        ] {
            env.insert(key.into(), placeholder.clone());
        }
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

    #[test]
    fn proxy_environment_never_projects_upstream_credentials() {
        std::env::set_var("DEEPSEEK_API_KEY", "host-secret");
        let env = proxy_environment("127.0.0.1:8080", "sess-1");
        assert!(!env.contains_key("DEEPSEEK_API_KEY"));
        assert!(!env.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        std::env::remove_var("DEEPSEEK_API_KEY");
    }

    #[test]
    fn local_auth_uses_run_scoped_placeholders_not_host_secrets() {
        std::env::set_var("OPENAI_API_KEY", "host-secret");
        let env = proxy_environment_with_local_auth("127.0.0.1:8080", "sess-1", true);
        assert_eq!(
            env.get("OPENAI_API_KEY").map(String::as_str),
            Some("persisting-local-sess-1")
        );
        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some("persisting-local-sess-1")
        );
        std::env::remove_var("OPENAI_API_KEY");
    }
}
