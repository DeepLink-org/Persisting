//! Model name → route selection and `forward` resolution.

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use serde_json::Value;

use crate::config::{ModelRoute, ProxyConfig};

/// Resolved upstream route after optional `forward`.
pub struct ResolvedRoute<'a> {
    pub client_model: String,
    pub upstream_model: String,
    pub route: &'a ModelRoute,
    /// Request body `model` differs from client (via `forward`).
    pub model_rewritten: bool,
}

pub fn select_route<'a>(routes: &'a [ModelRoute], model: &str) -> Option<&'a ModelRoute> {
    routes.iter().find(|r| model_matches(&r.name, model))
}

pub fn resolve_route<'a>(
    routes: &'a [ModelRoute],
    client_model: &str,
) -> Result<ResolvedRoute<'a>> {
    let matched = select_route(routes, client_model)
        .ok_or_else(|| anyhow::anyhow!("no route for model {client_model}"))?;

    if let Some(ref target_name) = matched.forward {
        let target = routes
            .iter()
            .find(|r| r.name == *target_name)
            .ok_or_else(|| anyhow::anyhow!("forward target `{target_name}` not found"))?;
        if target.forward.is_some() {
            bail!("forward target `{target_name}` cannot forward again");
        }
        if target.upstream.is_none() {
            bail!("forward target `{target_name}` missing upstream");
        }
        let upstream_model = target_name.clone();
        let model_rewritten = upstream_model != client_model;
        return Ok(ResolvedRoute {
            client_model: client_model.to_string(),
            upstream_model,
            route: target,
            model_rewritten,
        });
    }

    if matched.upstream.is_none() {
        bail!(
            "models[] entry `{}` has no upstream (set `upstream` or `forward`)",
            matched.name
        );
    }

    Ok(ResolvedRoute {
        client_model: client_model.to_string(),
        upstream_model: client_model.to_string(),
        route: matched,
        model_rewritten: false,
    })
}

pub fn resolve_route_config<'a>(
    cfg: &'a ProxyConfig,
    client_model: &str,
) -> Result<ResolvedRoute<'a>> {
    resolve_route(&cfg.models, client_model)
}

/// Replace JSON `model` field before sending upstream.
pub fn rewrite_model_in_body(body: &Bytes, upstream_model: &str) -> Result<Bytes> {
    if body.is_empty() {
        return Ok(Bytes::new());
    }
    let mut v: Value = serde_json::from_slice(body).context("parse request JSON")?;
    let Some(obj) = v.as_object_mut() else {
        bail!("request body must be a JSON object to rewrite model");
    };
    obj.insert(
        "model".to_string(),
        Value::String(upstream_model.to_string()),
    );
    Ok(Bytes::from(
        serde_json::to_vec(&v).context("serialize request JSON")?,
    ))
}

pub fn model_matches(pattern: &str, model: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return !prefix.is_empty() && model.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return !suffix.is_empty() && model.ends_with(suffix);
    }
    pattern == model
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyConfig;

    fn target(name: &str, upstream: &str) -> ModelRoute {
        ModelRoute {
            name: name.into(),
            provider: None,
            upstream: Some(upstream.into()),
            upstream_anthropic: None,
            path_prefix: None,
            api_key_env: None,
            api_key: None,
            forward: None,
        }
    }

    fn forward_rule(match_pat: &str, to: &str) -> ModelRoute {
        ModelRoute {
            name: match_pat.into(),
            provider: None,
            upstream: None,
            upstream_anthropic: None,
            path_prefix: None,
            api_key_env: None,
            api_key: None,
            forward: Some(to.into()),
        }
    }

    #[test]
    fn wildcard_star() {
        assert!(model_matches("*", "gpt-4o"));
    }

    #[test]
    fn prefix_wildcard() {
        assert!(model_matches("deepseek*", "deepseek-chat"));
        assert!(!model_matches("deepseek*", "gpt-4"));
    }

    #[test]
    fn exact() {
        assert!(model_matches("claude-3", "claude-3"));
        assert!(!model_matches("claude-3", "claude-4"));
    }

    #[test]
    fn passthrough_uses_matched_upstream() {
        let routes = vec![target("deepseek-chat", "http://ds/v1")];
        let r = resolve_route(&routes, "deepseek-chat").unwrap();
        assert!(!r.model_rewritten);
        assert_eq!(r.upstream_model, "deepseek-chat");
        assert_eq!(r.route.upstream.as_deref(), Some("http://ds/v1"));
    }

    #[test]
    fn forward_rewrites_to_target() {
        let routes = vec![
            target("deepseek-chat", "http://ds/v1"),
            forward_rule("claude-*", "deepseek-chat"),
        ];
        let r = resolve_route(&routes, "claude-sonnet-4").unwrap();
        assert!(r.model_rewritten);
        assert_eq!(r.client_model, "claude-sonnet-4");
        assert_eq!(r.upstream_model, "deepseek-chat");
        assert_eq!(r.route.name, "deepseek-chat");
    }

    #[test]
    fn first_match_wins() {
        let routes = vec![
            target("deepseek-chat", "http://ds/v1"),
            forward_rule("*", "deepseek-chat"),
        ];
        let r = resolve_route(&routes, "deepseek-chat").unwrap();
        assert!(!r.model_rewritten);
    }

    #[test]
    fn rewrite_model_in_request_body() {
        let body = Bytes::from_static(br#"{"model":"claude-3","messages":[]}"#);
        let out = super::rewrite_model_in_body(&body, "deepseek-chat").unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "deepseek-chat");
    }

    #[test]
    fn rewrite_model_empty_body_is_noop() {
        let out = super::rewrite_model_in_body(&Bytes::new(), "deepseek-chat").unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn config_validate_forward() {
        let yaml = r#"
listen: "127.0.0.1:1"
models:
  - name: deepseek-chat
    upstream: "http://ds/v1"
  - name: "claude-*"
    forward: deepseek-chat
"#;
        ProxyConfig::from_yaml_str(yaml).unwrap();
    }

    #[test]
    fn config_rejects_forward_and_upstream() {
        let yaml = r#"
listen: "127.0.0.1:1"
models:
  - name: bad
    upstream: "http://x/v1"
    forward: other
"#;
        assert!(ProxyConfig::from_yaml_str(yaml).is_err());
    }
}
