use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const STORAGE_KEY: &str = "pchronicle_llm_config";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRequest {
    pub system: String,
    pub messages: Vec<Value>,
    pub tools: Option<Value>,
    pub response_format: Option<Value>,
    pub temperature: f64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            api_base: "https://api.deepseek.com/v1".into(),
            api_key: String::new(),
            model: "deepseek-chat".into(),
        }
    }
}

impl LlmConfig {
    pub fn is_configured(&self) -> bool {
        !self.api_base.trim().is_empty()
            && !self.api_key.trim().is_empty()
            && !self.model.trim().is_empty()
    }
}

pub fn load_config() -> LlmConfig {
    let Some(window) = web_sys::window() else {
        return LlmConfig::default();
    };
    let Some(storage) = window.local_storage().ok().flatten() else {
        return LlmConfig::default();
    };
    storage
        .get_item(STORAGE_KEY)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_config(config: &LlmConfig) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(storage) = window.local_storage().ok().flatten() else {
        return;
    };
    if let Ok(raw) = serde_json::to_string(config) {
        let _ = storage.set_item(STORAGE_KEY, &raw);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompletionError {
    pub status: Option<u16>,
    pub message: String,
}

impl CompletionError {
    pub fn suggests_tools_unsupported(&self) -> bool {
        matches!(self.status, Some(400 | 422))
            && ["tools", "tool_choice", "function", "response_format"]
                .iter()
                .any(|needle| self.message.to_ascii_lowercase().contains(needle))
    }

    pub fn suggests_response_format_unsupported(&self) -> bool {
        matches!(self.status, Some(400 | 422))
            && self
                .message
                .to_ascii_lowercase()
                .contains("response_format")
    }
}

pub fn completion_body(model: &str, request: CompletionRequest) -> Value {
    let mut messages = vec![json!({"role":"system", "content": request.system})];
    messages.extend(request.messages);
    let mut body = json!({
        "model": model.trim(),
        "temperature": request.temperature,
        "messages": messages,
    });
    if let Some(tools) = request.tools {
        body["tools"] = tools;
        body["tool_choice"] = json!("auto");
    }
    if let Some(response_format) = request.response_format {
        body["response_format"] = response_format;
    }
    body
}

pub async fn complete(
    config: &LlmConfig,
    request: CompletionRequest,
) -> Result<Value, CompletionError> {
    let url = format!(
        "{}/chat/completions",
        config.api_base.trim().trim_end_matches('/')
    );
    let body = completion_body(&config.model, request);
    let response = gloo_net::http::Request::post(&url)
        .header(
            "Authorization",
            &format!("Bearer {}", config.api_key.trim()),
        )
        .header("Content-Type", "application/json")
        .json(&body)
        .map_err(|error| CompletionError {
            status: None,
            message: error.to_string(),
        })?
        .send()
        .await
        .map_err(|error| CompletionError {
            status: None,
            message: format!("LLM request failed (check API base, key, and CORS): {error}"),
        })?;
    let status = response.status();
    let raw = response.text().await.map_err(|error| CompletionError {
        status: Some(status),
        message: error.to_string(),
    })?;
    if !(200..300).contains(&status) {
        return Err(CompletionError {
            status: Some(status),
            message: format!("LLM HTTP {status}: {raw}"),
        });
    }
    let value: Value = serde_json::from_str(&raw).map_err(|error| CompletionError {
        status: Some(status),
        message: format!("LLM returned invalid JSON: {error}"),
    })?;
    let message = value
        .pointer("/choices/0/message")
        .cloned()
        .ok_or_else(|| CompletionError {
            status: Some(status),
            message: "LLM returned an empty response".into(),
        })?;
    let has_content = message
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.trim().is_empty());
    let has_tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty());
    if !has_content && !has_tool_calls {
        return Err(CompletionError {
            status: Some(status),
            message: "LLM returned an empty response".into(),
        });
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_body_omits_optional_fields() {
        let body = completion_body(
            "model-a",
            CompletionRequest {
                system: "system".into(),
                messages: vec![json!({"role":"user","content":"question"})],
                tools: None,
                response_format: None,
                temperature: 0.2,
            },
        );
        assert_eq!(body["model"], "model-a");
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert!(body.get("tools").is_none());
        assert!(body.get("response_format").is_none());
    }

    #[test]
    fn completion_body_includes_tools_and_json_contract() {
        let body = completion_body(
            "model-a",
            CompletionRequest {
                system: "system".into(),
                messages: Vec::new(),
                tools: Some(json!([{"type":"function"}])),
                response_format: Some(json!({"type":"json_object"})),
                temperature: 0.1,
            },
        );
        assert!(body.get("tools").is_some());
        assert_eq!(body["response_format"]["type"], "json_object");
    }
}
