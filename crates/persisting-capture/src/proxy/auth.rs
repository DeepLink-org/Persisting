//! Upstream API key resolution (OpenAI Bearer vs Anthropic x-api-key).

use axum::http::HeaderMap;
use reqwest::RequestBuilder;

use crate::config::ModelRoute;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::proxy::http_headers::skip_upstream_forward_header;

pub fn resolve_upstream_api_key(
    route: &ModelRoute,
    client_headers: &HeaderMap,
) -> anyhow::Result<(Option<String>, &'static str)> {
    if let Some(k) = route.api_key_value()? {
        let source = route
            .api_key_env
            .as_deref()
            .map(|_| "daemon_env")
            .unwrap_or("inline_api_key");
        return Ok((Some(k), source));
    }
    if let Some(k) = client_auth_token(client_headers) {
        return Ok((Some(k), "client_header"));
    }
    if route.api_key_env.is_some() || route.api_key.is_some() {
        anyhow::bail!(
            "api key not available for model route `{}` (set {} or pass client auth header)",
            route.name,
            route.api_key_env.as_deref().unwrap_or("api_key in config")
        );
    }
    Ok((None, "none"))
}

/// Forward client headers and attach route API key with the correct scheme.
pub fn apply_upstream_headers(
    mut req: RequestBuilder,
    client_headers: &HeaderMap,
    route: &ModelRoute,
    protocol: ProtocolKind,
) -> anyhow::Result<RequestBuilder> {
    let provider = route.provider_kind();
    let anthropic_style = provider == ProviderKind::Anthropic || protocol == ProtocolKind::Messages;

    for (name, value) in client_headers.iter() {
        if skip_upstream_forward_header(name.as_str()) {
            continue;
        }
        req = req.header(name, value);
    }

    let (key, _source) = resolve_upstream_api_key(route, client_headers)?;

    if let Some(key) = key {
        if anthropic_style {
            req = req.header("x-api-key", key);
        } else {
            req = req.bearer_auth(key);
        }
    } else if route.api_key_env.is_some() || route.api_key.is_some() {
        anyhow::bail!(
            "api key not available for model route `{}` (set {} or pass client auth header)",
            route.name,
            route.api_key_env.as_deref().unwrap_or("api_key in config")
        );
    }

    Ok(req)
}

/// Client-side LLM credentials (Claude Code → `x-api-key` or `Authorization: Bearer`).
fn client_auth_token(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn route(provider: Option<&str>, key: Option<&str>) -> ModelRoute {
        route_with_env(provider, key, None)
    }

    fn route_with_env(
        provider: Option<&str>,
        api_key: Option<&str>,
        api_key_env: Option<&str>,
    ) -> ModelRoute {
        ModelRoute {
            name: "*".into(),
            provider: provider.map(str::to_string),
            upstream: Some("https://example.com/v1".into()),
            upstream_anthropic: None,
            path_prefix: None,
            api_key_env: api_key_env.map(str::to_string),
            api_key: api_key.map(str::to_string),
            forward: None,
        }
    }

    fn clear_api_key_env() {
        for key in [
            "DEEPSEEK_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
        ] {
            unsafe {
                std::env::remove_var(key);
            }
        }
    }

    #[test]
    fn anthropic_uses_x_api_key() {
        let r = route(Some("anthropic"), Some("sk-test"));
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer client"));
        headers.insert("x-api-key", HeaderValue::from_static("client-key"));
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

        let rb = apply_upstream_headers(
            reqwest::Client::new().post("https://example.com/v1/messages"),
            &headers,
            &r,
            ProtocolKind::Messages,
        )
        .unwrap();
        let built = rb.build().unwrap();
        assert_eq!(built.headers().get("x-api-key").unwrap(), "sk-test");
        assert!(built.headers().get("authorization").is_none());
        assert_eq!(
            built.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
    }

    #[test]
    fn openai_uses_bearer() {
        let r = route(Some("openai"), Some("sk-test"));
        let headers = HeaderMap::new();
        let rb = apply_upstream_headers(
            reqwest::Client::new().post("https://example.com/v1/chat/completions"),
            &headers,
            &r,
            ProtocolKind::ChatCompletions,
        )
        .unwrap();
        let built = rb.build().unwrap();
        assert_eq!(
            built.headers().get("authorization").unwrap(),
            "Bearer sk-test"
        );
    }

    #[test]
    fn falls_back_to_client_x_api_key_when_daemon_env_missing() {
        clear_api_key_env();
        let r = route_with_env(Some("anthropic"), None, Some("DEEPSEEK_API_KEY"));
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("sk-from-claude"));
        let rb = apply_upstream_headers(
            reqwest::Client::new().post("https://example.com/v1/messages"),
            &headers,
            &r,
            ProtocolKind::Messages,
        )
        .unwrap();
        let built = rb.build().unwrap();
        assert_eq!(built.headers().get("x-api-key").unwrap(), "sk-from-claude");
    }

    #[test]
    fn falls_back_to_client_bearer_for_anthropic_upstream() {
        clear_api_key_env();
        let r = route_with_env(Some("anthropic"), None, Some("DEEPSEEK_API_KEY"));
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer sk-from-claude"),
        );
        let rb = apply_upstream_headers(
            reqwest::Client::new().post("https://example.com/v1/messages"),
            &headers,
            &r,
            ProtocolKind::Messages,
        )
        .unwrap();
        let built = rb.build().unwrap();
        assert_eq!(built.headers().get("x-api-key").unwrap(), "sk-from-claude");
    }
}
