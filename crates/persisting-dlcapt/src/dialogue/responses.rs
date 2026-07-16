use serde_json::Value;

pub fn extract_user_from_responses_input(body: &Value) -> Option<String> {
    let input = body.get("input")?;
    match input {
        Value::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        Value::Array(items) => extract_tail_user_text(items),
        _ => None,
    }
}

fn extract_tail_user_text(items: &[Value]) -> Option<String> {
    for item in items.iter().rev() {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        if item_type == "function_call_output" {
            continue;
        }
        if item.get("role").and_then(Value::as_str) == Some("user")
            && let Some(text) = content_to_text(item.get("content"))
        {
            return Some(text);
        }
        if let Some(text) = content_to_text(Some(item)) {
            return Some(text);
        }
    }
    None
}

fn content_to_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    match content {
        Value::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        Value::Array(parts) => {
            let mut texts = Vec::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        texts.push(text.to_string());
                    }
                } else if part.get("type").and_then(Value::as_str) == Some("input_text")
                    && let Some(text) = part.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    texts.push(text.to_string());
                }
            }
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            }
        }
        Value::Object(map) if map.contains_key("content") => content_to_text(map.get("content")),
        _ => None,
    }
}

pub fn summarize_responses_json_response(value: &Value) -> (Option<String>, Option<Value>) {
    let usage = value.get("usage").cloned();
    if let Some(text) = value.get("output_text").and_then(Value::as_str)
        && !text.is_empty()
    {
        return (Some(text.to_string()), usage);
    }

    let mut texts = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(text) = part.get("text").and_then(Value::as_str)
                            {
                                texts.push(text.to_string());
                            }
                        }
                    }
                }
                Some("output_text") => {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        texts.push(text.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    let response_text = if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n"))
    };
    (response_text, usage)
}

pub fn summarize_responses_sse_response(raw: &str) -> (Option<String>, Option<Value>) {
    let mut content = String::new();
    let mut usage = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let chunk = match serde_json::from_str::<Value>(data) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match chunk.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = chunk.get("delta").and_then(Value::as_str) {
                    content.push_str(delta);
                }
            }
            Some("response.output_text.done") => {
                if content.is_empty()
                    && let Some(text) = chunk.get("text").and_then(Value::as_str)
                {
                    content.push_str(text);
                }
            }
            _ => {}
        }

        if usage.is_none() {
            usage = chunk
                .get("response")
                .and_then(|response| response.get("usage"))
                .cloned()
                .or_else(|| chunk.get("usage").cloned());
        }
    }

    let response_text = if content.is_empty() {
        None
    } else {
        Some(content)
    };
    (response_text, usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_user_from_string_input() {
        let body = json!({"input": "hello responses"});
        assert_eq!(
            extract_user_from_responses_input(&body).as_deref(),
            Some("hello responses")
        );
    }

    #[test]
    fn summarize_responses_json_should_read_output_text() {
        let body = json!({
            "output_text": "assistant reply",
            "usage": {"total_tokens": 10}
        });
        let (text, usage) = summarize_responses_json_response(&body);
        assert_eq!(text.as_deref(), Some("assistant reply"));
        assert!(usage.is_some());
    }
}
