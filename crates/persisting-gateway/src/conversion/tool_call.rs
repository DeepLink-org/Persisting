//! Shared tool-call helpers (moon-bridge Core `tool_use` / `tool_result` subset).

use serde_json::{json, Value};

/// Responses input types that represent an outbound tool invocation.
pub fn is_tool_call_input_type(item_type: &str) -> bool {
    matches!(
        item_type,
        "function_call" | "custom_tool_call" | "local_shell_call"
    )
}

/// Responses input types that carry tool execution results back to the model.
pub fn is_tool_call_output_type(item_type: &str) -> bool {
    matches!(
        item_type,
        "function_call_output" | "custom_tool_call_output" | "local_shell_call_output"
    )
}

/// Build a Chat Completions `tool_calls[]` entry from a Responses input item.
pub fn chat_tool_call_from_input_item(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("call_proxy");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = tool_arguments_from_input_item(item);
    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments
        }
    })
}

fn tool_arguments_from_input_item(item: &Value) -> String {
    if let Some(s) = item.get("arguments").and_then(|v| v.as_str()) {
        return normalize_tool_arguments_string(s);
    }
    if let Some(input) = item.get("input").and_then(|v| v.as_str()) {
        return serde_json::to_string(&json!({"input": input}))
            .unwrap_or_else(|_| "{}".to_string());
    }
    if let Some(action) = item.get("action") {
        return serde_json::to_string(action).unwrap_or_else(|_| "{}".to_string());
    }
    "{}".to_string()
}

/// Normalize tool arguments for Chat Completions (must be a JSON object string).
pub fn normalize_tool_arguments_string(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }
    "{}".to_string()
}

/// Decode a streaming `function.arguments` delta chunk.
///
/// Some providers JSON-encode argument fragments; unwrap once when the chunk is
/// a quoted JSON string (moon-bridge `unquoteArguments` streaming variant).
pub fn decode_stream_arguments_delta(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if raw.starts_with('"') {
        if let Ok(decoded) = serde_json::from_str::<String>(raw) {
            return decoded;
        }
    }
    raw.to_string()
}

/// Unwrap double-encoded tool arguments from a non-streaming Chat message.
pub fn unquote_chat_tool_arguments(raw: &Value) -> String {
    match raw {
        Value::String(s) => normalize_tool_arguments_string(s),
        Value::Null => "{}".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_tool_call_types() {
        assert!(is_tool_call_input_type("function_call"));
        assert!(is_tool_call_input_type("custom_tool_call"));
        assert!(!is_tool_call_output_type("function_call"));
        assert!(is_tool_call_output_type("function_call_output"));
    }

    #[test]
    fn chat_tool_call_from_responses_item() {
        let item = json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "shell",
            "arguments": "{\"command\":\"ls\"}"
        });
        let tc = chat_tool_call_from_input_item(&item);
        assert_eq!(tc["function"]["name"], "shell");
        assert_eq!(tc["id"], "call_1");
    }

    #[test]
    fn decode_stream_arguments_delta_passthrough() {
        assert_eq!(decode_stream_arguments_delta("{\"cmd\""), "{\"cmd\"");
    }
}
