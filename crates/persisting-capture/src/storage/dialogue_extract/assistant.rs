//! Cross-protocol assistant response extraction (SSE + JSON).

use serde_json::Value;

use super::completions::{completions_assistant_from_json, completions_sse_turn};
use super::messages::{anthropic_assistant_from_json, anthropic_sse_turn};
use super::responses::{responses_assistant_from_json, responses_sse_turn};

/// Visible assistant turn from SSE: Anthropic, OpenAI completions, or Responses events.
pub fn extract_assistant_turn_from_sse(raw: &str) -> String {
    let anthropic = anthropic_sse_turn(raw);
    if !anthropic.is_empty() {
        return anthropic;
    }
    let completions = completions_sse_turn(raw);
    if !completions.is_empty() {
        return completions;
    }
    responses_sse_turn(raw)
}

/// Assistant visible text from a JSON response body (OpenAI `choices` or Anthropic `content`).
pub fn extract_assistant_text_from_json(body: &Value) -> Option<String> {
    if let Some(s) = body.as_str() {
        if s.contains("event:") && s.contains("data:") {
            let text = extract_assistant_turn_from_sse(s);
            if !text.is_empty() {
                return Some(text);
            }
        }
        return None;
    }
    completions_assistant_from_json(body)
        .or_else(|| anthropic_assistant_from_json(body))
        .or_else(|| responses_assistant_from_json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_sse_extracts_text_not_thinking() {
        let sse = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"secret"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hi"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"!"}}
"#;
        assert_eq!(extract_assistant_turn_from_sse(sse), "Hi!");
    }

    #[test]
    fn completions_sse_extracts_multiple_tool_calls() {
        let sse = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\": \"a.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"name":"read_file","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\": \"b.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}
"#;
        let out = extract_assistant_turn_from_sse(sse);
        assert!(out.contains("```tool:read_file"));
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
    }
}
