mod responses;

use serde_json::Value;

pub use responses::{
    extract_user_from_responses_input, summarize_responses_json_response,
    summarize_responses_sse_response,
};

pub fn extract_last_user_message(body: &Value) -> Option<String> {
    let messages = body.get("messages")?.as_array()?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        if let Some(text) = msg.get("content").and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(text.to_string());
            }
            continue;
        }
        if let Some(parts) = msg.get("content").and_then(Value::as_array) {
            let mut texts = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        texts.push(text.to_string());
                    }
                }
            }
            if !texts.is_empty() {
                return Some(texts.join("\n"));
            }
        }
    }
    None
}

pub fn extract_user_text(endpoint: InferenceEndpoint, body: &Value) -> Option<String> {
    match endpoint {
        InferenceEndpoint::ChatCompletions => extract_last_user_message(body),
        InferenceEndpoint::Responses => extract_user_from_responses_input(body),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceEndpoint {
    ChatCompletions,
    Responses,
}

impl InferenceEndpoint {
    pub fn upstream_suffix(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat/completions",
            Self::Responses => "responses",
        }
    }
}
