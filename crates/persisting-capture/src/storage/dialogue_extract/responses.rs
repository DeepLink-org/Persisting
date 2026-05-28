//! OpenAI Responses API: `input` / `output` extraction and SSE events.

use std::collections::BTreeMap;

use serde_json::Value;

use super::common::{
    format_tool_result_block, is_codex_context_injection, join_assistant_parts, unwrap_session_tag,
};

pub(crate) fn extract_user_from_responses_input(v: &Value) -> Option<String> {
    let input = v.get("input")?;
    match input {
        Value::String(s) if !s.trim().is_empty() => {
            let s = unwrap_session_tag(s);
            if is_codex_context_injection(s) {
                None
            } else {
                Some(s.to_string())
            }
        }
        Value::Array(items) => extract_tail_from_responses_input(items),
        _ => None,
    }
}

pub(crate) fn count_responses_input_user_turns(input: &Value) -> usize {
    match input {
        Value::String(s) => {
            let s = unwrap_session_tag(s);
            if is_codex_context_injection(s) || s.trim().is_empty() {
                0
            } else {
                1
            }
        }
        Value::Array(items) => items
            .iter()
            .filter(|item| responses_input_item_is_user_turn(item))
            .count(),
        _ => 0,
    }
}

fn responses_input_item_is_user_turn(item: &Value) -> bool {
    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match item_type {
        "function_call" | "function_call_output" => false,
        "message" => {
            item.get("role").and_then(|r| r.as_str()) == Some("user")
                && last_non_injection_text_from_content(item.get("content")).is_some()
        }
        _ => last_non_injection_text_from_content(item.get("content")).is_some(),
    }
}

fn extract_tail_from_responses_input(items: &[Value]) -> Option<String> {
    let mut outputs = Vec::new();
    for item in items.iter().rev() {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("function_call_output") => {
                if let Some(formatted) = format_responses_function_call_output(item) {
                    outputs.push(formatted);
                }
            }
            _ => break,
        }
    }
    if !outputs.is_empty() {
        outputs.reverse();
        return Some(outputs.join("\n\n"));
    }
    for item in items.iter().rev() {
        if let Some(text) = last_user_text_from_input_item(item) {
            return Some(text);
        }
    }
    None
}

fn format_responses_function_call_output(item: &Value) -> Option<String> {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("call");
    let output = item.get("output")?;
    let content = responses_output_to_text(output);
    if content.trim().is_empty() {
        return None;
    }
    Some(format!("```tool_result:{call_id}\n{content}\n```"))
}

fn responses_output_to_text(output: &Value) -> String {
    match output {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match t {
                    "input_text" | "text" | "output_text" => part
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    _ => part
                        .get("text")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn last_user_text_from_input_item(item: &Value) -> Option<String> {
    let item_type = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match item_type {
        "function_call" | "function_call_output" => None,
        "message" => {
            let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            if role != "user" {
                return None;
            }
            last_non_injection_text_from_content(item.get("content"))
        }
        _ => last_non_injection_text_from_content(item.get("content")),
    }
}

fn last_non_injection_text_from_content(content: Option<&Value>) -> Option<String> {
    let content = content?;
    match content {
        Value::String(s) => {
            let s = unwrap_session_tag(s);
            if is_codex_context_injection(s) {
                None
            } else {
                Some(s.to_string())
            }
        }
        Value::Array(parts) => {
            for part in parts.iter().rev() {
                let t = part.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match t {
                    "input_text" | "text" => {
                        if let Some(text) = part.get("text").and_then(|x| x.as_str()) {
                            let text = unwrap_session_tag(text);
                            if !is_codex_context_injection(text) && !text.trim().is_empty() {
                                return Some(text.to_string());
                            }
                        }
                    }
                    "tool_result" | "function_call_output" => {
                        if let Some(formatted) = format_tool_result_block(part) {
                            return Some(formatted);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn responses_sse_turn(raw: &str) -> String {
    #[derive(Default)]
    struct ToolAccum {
        name: String,
        arguments: String,
    }

    let mut text = String::new();
    let mut tool_calls: BTreeMap<u32, ToolAccum> = BTreeMap::new();
    let mut next_tool_index = 0u32;

    for line in raw.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                    text.push_str(delta);
                }
            }
            "response.output_text.done" => {
                if let Some(done) = v.get("text").and_then(|t| t.as_str()) {
                    if text.is_empty() {
                        text.push_str(done);
                    }
                }
            }
            "response.output_item.added" => {
                if v.get("item")
                    .and_then(|i| i.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("function_call")
                {
                    let item = v.get("item").unwrap();
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    let output_index =
                        v.get("output_index")
                            .and_then(|i| i.as_u64())
                            .unwrap_or(next_tool_index as u64) as u32;
                    next_tool_index = next_tool_index.max(output_index + 1);
                    tool_calls.entry(output_index).or_default().name = name;
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = v.get("delta").and_then(|d| d.as_str()) {
                    let output_index = v
                        .get("output_index")
                        .and_then(|i| i.as_u64())
                        .map(|i| i as u32);
                    if let Some(idx) = output_index {
                        tool_calls.entry(idx).or_default().arguments.push_str(delta);
                    } else if let Some((_, entry)) = tool_calls.iter_mut().next_back() {
                        entry.arguments.push_str(delta);
                    }
                }
            }
            "response.function_call_arguments.done" => {
                let args = v
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}")
                    .to_string();
                let output_index = v
                    .get("output_index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as u32);
                if let Some(idx) = output_index {
                    tool_calls.entry(idx).or_default().arguments = args;
                } else if let Some((_, entry)) = tool_calls.iter_mut().next_back() {
                    entry.arguments = args;
                }
            }
            _ => {}
        }
    }

    join_assistant_parts(
        &text,
        tool_calls
            .values()
            .map(|t| (t.name.clone(), t.arguments.clone())),
    )
}

pub(crate) fn responses_assistant_from_json(body: &Value) -> Option<String> {
    let output = body.get("output")?.as_array()?;
    let mut texts = Vec::new();
    for item in output {
        match item.get("type").and_then(|t| t.as_str()) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for part in parts {
                        if part.get("type").and_then(|t| t.as_str()) == Some("output_text") {
                            if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                                texts.push(t.to_string());
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let args = item
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .unwrap_or("{}");
                texts.push(format!("```tool:{name}\n{args}\n```"));
            }
            _ => {}
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use serde_json::json;

    #[test]
    fn codex_responses_input_extracts_tail_function_call_output() {
        let body = serde_json::to_vec(&json!({
            "model": "gpt-5.5",
            "input": [
                {"type":"message","role":"user","content":[{"type":"input_text","text":"review"}]},
                {"type":"function_call","call_id":"call_1","name":"exec_command","arguments":"{}"},
                {"type":"function_call_output","call_id":"call_1","output":"Cargo.toml\nsrc/"}
            ]
        }))
        .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let out = extract_user_from_responses_input(&v).unwrap();
        assert!(out.contains("```tool_result:call_1"));
        assert!(out.contains("Cargo.toml"));
    }

    #[test]
    fn codex_responses_input_extracts_last_user_text() {
        let body = Bytes::from_static(include_bytes!(
            "../../../tests/fixtures/local/requests/responses/codex_basic.json"
        ));
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(extract_user_from_responses_input(&v).as_deref(), Some("hi"));
    }

    #[test]
    fn responses_sse_extracts_output_text_delta() {
        let sse = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Hello"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":" world"}
"#;
        assert_eq!(responses_sse_turn(sse), "Hello world");
    }
}
