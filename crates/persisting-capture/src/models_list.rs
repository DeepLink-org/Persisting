//! OpenAI-compatible `/v1/models` stub from proxy config.

use serde_json::{json, Value};

use crate::config::{ModelRoute, ProxyConfig};
use crate::router::model_matches;

/// Build model ids exposed to clients (OpenAI list format).
pub fn build_models_response(cfg: &ProxyConfig) -> Value {
    let mut ids = Vec::new();
    for route in &cfg.models {
        collect_model_ids(route, &cfg.models, &mut ids);
    }
    ids.sort();
    ids.dedup();
    let created = chrono::Utc::now().timestamp();
    let data = ids
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "object": "model",
                "created": created,
                "owned_by": cfg.agent_id,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "object": "list",
        "data": data,
    })
}

fn collect_model_ids(route: &ModelRoute, all: &[ModelRoute], out: &mut Vec<String>) {
    if route.upstream.is_some() {
        push_id(out, &route.name);
    }
    if route.forward.is_some() {
        return;
    }
    if route.upstream.is_none() {
        return;
    }
    // Expose match patterns as discoverable ids when not wildcard-only routing entry.
    if route.name.contains('*') {
        for other in all {
            if other.name == route.name {
                continue;
            }
            if !other.name.contains('*') && other.upstream.is_some() {
                push_id(out, &other.name);
            }
        }
    }
}

fn push_id(out: &mut Vec<String>, id: &str) {
    if id == "*" || id.contains('*') {
        return;
    }
    out.push(id.to_string());
}

/// Match patterns that would answer for a given model id (for stub validation).
pub fn config_lists_model(cfg: &ProxyConfig, model_id: &str) -> bool {
    cfg.models.iter().any(|r| {
        if r.name == model_id && r.upstream.is_some() {
            return true;
        }
        model_matches(&r.name, model_id) && r.upstream.is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProxyConfig;

    #[test]
    fn models_list_from_config() {
        let cfg = ProxyConfig::from_yaml_str(
            r#"
listen: "127.0.0.1:1"
models:
  - name: deepseek-chat
    upstream: "http://x/v1"
  - name: "*"
    forward: deepseek-chat
"#,
        )
        .unwrap();
        let v = build_models_response(&cfg);
        let ids: Vec<_> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&"deepseek-chat"));
    }
}
