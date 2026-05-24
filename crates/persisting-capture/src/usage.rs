//! Token usage extraction and rough USD cost estimation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ProviderKind;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// Streaming capture metrics (TTFT + incremental usage).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamMetrics {
    pub usage: TokenUsage,
    pub ttft_ms: Option<u64>,
}

impl TokenUsage {
    pub fn merge(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

/// Extract usage from OpenAI-style `usage` or Anthropic `usage` blocks.
/// Merge usage from SSE `data:` lines (OpenAI / Anthropic streaming).
pub fn extract_usage_from_sse(raw: &str) -> TokenUsage {
    let mut merged = TokenUsage::default();
    for line in raw.lines() {
        let line = line.trim();
        let Some(json_str) = line.strip_prefix("data:") else {
            continue;
        };
        let json_str = json_str.trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(json_str) {
            merged.merge(&extract_usage_from_response(&v));
            if let Some(msg) = v.get("message") {
                merged.merge(&extract_usage_from_response(msg));
            }
        }
    }
    merged
}

pub fn extract_usage_from_response(body: &Value) -> TokenUsage {
    let mut u = TokenUsage::default();
    let usage = body.get("usage").or_else(|| body.get("Usage"));
    let Some(usage) = usage else {
        return u;
    };
    u.input_tokens = pick_u64(usage, &["prompt_tokens", "input_tokens"]);
    u.output_tokens = pick_u64(usage, &["completion_tokens", "output_tokens"]);
    u.total_tokens = pick_u64(usage, &["total_tokens"]);
    if u.total_tokens == 0 {
        u.total_tokens = u.input_tokens + u.output_tokens;
    }
    if let Some(details) = usage.get("prompt_tokens_details") {
        u.cache_read_tokens = pick_u64(details, &["cached_tokens"]);
    }
    if let Some(details) = usage.get("cache_creation_input_tokens") {
        u.cache_write_tokens = pick_u64(details, &[]);
    }
    if usage.get("cache_creation_input_tokens").is_some() {
        u.cache_write_tokens = pick_u64(usage, &["cache_creation_input_tokens"]);
    }
    if let Some(details) = usage.get("completion_tokens_details") {
        u.reasoning_tokens = pick_u64(details, &["reasoning_tokens"]);
    }
    if usage.get("cache_read_input_tokens").is_some() {
        u.cache_read_tokens = pick_u64(usage, &["cache_read_input_tokens"]);
    }
    u
}

fn pick_u64(v: &Value, keys: &[&str]) -> u64 {
    for k in keys {
        if let Some(n) = v.get(*k).and_then(|x| x.as_u64()) {
            return n;
        }
    }
    0
}

/// Per-million-token USD list prices (rough public list; override via config later).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl Default for ModelPricing {
    fn default() -> Self {
        Self {
            input_per_million: 1.0,
            output_per_million: 2.0,
        }
    }
}

pub fn default_pricing_table() -> Vec<(&'static str, ModelPricing)> {
    vec![
        (
            "gpt-4o",
            ModelPricing {
                input_per_million: 2.5,
                output_per_million: 10.0,
            },
        ),
        (
            "gpt-4o-mini",
            ModelPricing {
                input_per_million: 0.15,
                output_per_million: 0.6,
            },
        ),
        (
            "claude-3-5-sonnet",
            ModelPricing {
                input_per_million: 3.0,
                output_per_million: 15.0,
            },
        ),
        (
            "deepseek-chat",
            ModelPricing {
                input_per_million: 0.27,
                output_per_million: 1.1,
            },
        ),
        (
            "gemini-2.0-flash",
            ModelPricing {
                input_per_million: 0.1,
                output_per_million: 0.4,
            },
        ),
    ]
}

pub fn estimate_cost_usd(model: &str, provider: ProviderKind, usage: &TokenUsage) -> f64 {
    let table = default_pricing_table();
    let model_key = model.to_ascii_lowercase();
    let fallback = ModelPricing {
        input_per_million: match provider {
            ProviderKind::Anthropic => 3.0,
            ProviderKind::Gemini | ProviderKind::Vertex => 0.15,
            _ => 1.0,
        },
        output_per_million: match provider {
            ProviderKind::Anthropic => 15.0,
            ProviderKind::Gemini | ProviderKind::Vertex => 0.6,
            _ => 2.0,
        },
    };
    let price = table
        .iter()
        .find(|(m, _)| model_key.contains(m))
        .map(|(_, p)| p)
        .unwrap_or(&fallback);
    (usage.input_tokens as f64 / 1_000_000.0) * price.input_per_million
        + (usage.output_tokens as f64 / 1_000_000.0) * price.output_per_million
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_usage() {
        let body = json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30
            }
        });
        let u = extract_usage_from_response(&body);
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn anthropic_sse_usage() {
        let sse = "event: message_delta\n\
                   data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":12}}\n\n";
        let u = extract_usage_from_sse(sse);
        assert_eq!(u.output_tokens, 12);
    }

    #[test]
    fn cost_positive() {
        let u = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            ..Default::default()
        };
        let c = estimate_cost_usd("gpt-4o", ProviderKind::OpenAi, &u);
        assert!(c > 2.0);
    }
}
