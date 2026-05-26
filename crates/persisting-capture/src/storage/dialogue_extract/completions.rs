//! OpenAI Chat Completions API: choices, tool calls, SSE deltas.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::conversion::{decode_stream_arguments_delta, unquote_chat_tool_arguments};

use super::common::join_assistant_parts;

/// Incremental OpenAI Completions SSE parser for tool-call partial capture (Codex upstream).
#[derive(Default)]
pub struct CompletionsSseToolParser {
    line_carry: String,
    text: String,
    tool_calls: BTreeMap<u32, ToolAccum>,
    last_snapshot: Option<String>,
}

#[derive(Default)]
struct ToolAccum {
    name: String,
    arguments: String,
}

impl CompletionsSseToolParser {
    /// Incremental snapshot for Anthropic-style partial capture only.
    pub fn push_chunk(&mut self, chunk: &str) -> Option<String> {
        self.ingest_chunk(chunk);
        let snapshot = self.snapshot();
        if snapshot.is_empty() {
            return None;
        }
        if self.last_snapshot.as_deref() == Some(snapshot.as_str()) {
            return None;
        }
        self.last_snapshot = Some(snapshot.clone());
        Some(snapshot)
    }

    pub fn finish(&mut self) {
        if !self.line_carry.is_empty() {
            let line = std::mem::take(&mut self.line_carry);
            self.process_data_line(line.trim());
        }
    }

    pub fn snapshot(&self) -> String {
        join_assistant_parts(
            &self.text,
            self.tool_calls
                .values()
                .map(|t| (t.name.clone(), t.arguments.clone())),
        )
    }

    fn ingest_chunk(&mut self, chunk: &str) {
        self.line_carry.push_str(chunk);
        while let Some(nl) = self.line_carry.find('\n') {
            let line = self.line_carry[..nl].trim().to_string();
            self.line_carry.drain(..=nl);
            self.process_data_line(&line);
        }
    }

    fn process_data_line(&mut self, line: &str) {
        let Some(data) = line.strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        let Some(choice) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        else {
            return;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                self.text.push_str(content);
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                self.merge_tool_call_deltas(tcs);
            }
        }
        if let Some(message) = choice.get("message") {
            if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                if self.text.is_empty() && !content.is_empty() {
                    self.text.push_str(content);
                }
            }
            if let Some(tcs) = message.get("tool_calls").and_then(|t| t.as_array()) {
                self.merge_complete_tool_calls(tcs);
            }
        }
    }

    fn merge_tool_call_deltas(&mut self, tcs: &[Value]) {
        for tc in tcs {
            let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let entry = self.tool_calls.entry(index).or_default();
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())
            {
                entry.name = name.to_string();
            }
            if let Some(args) = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .filter(|a| !a.is_empty())
            {
                entry
                    .arguments
                    .push_str(&decode_stream_arguments_delta(args));
            }
        }
    }

    fn merge_complete_tool_calls(&mut self, tcs: &[Value]) {
        for (index, tc) in tcs.iter().enumerate() {
            let idx = tc
                .get("index")
                .and_then(|i| i.as_u64())
                .unwrap_or(index as u64) as u32;
            let entry = self.tool_calls.entry(idx).or_default();
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .filter(|n| !n.is_empty())
            {
                entry.name = name.to_string();
            }
            if let Some(args) = tc.get("function").and_then(|f| f.get("arguments")) {
                entry.arguments = unquote_chat_tool_arguments(args);
            }
        }
    }
}

pub(crate) fn completions_sse_turn(raw: &str) -> String {
    let mut parser = CompletionsSseToolParser::default();
    parser.ingest_chunk(raw);
    parser.finish();
    parser.snapshot()
}

pub(crate) fn completions_assistant_from_json(body: &Value) -> Option<String> {
    if let Some(content) = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
    {
        if let Some(text) = super::messages::content_to_string(content) {
            return Some(text);
        }
    }
    if let Some(message) = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
    {
        return format_openai_tool_calls(message.get("tool_calls"));
    }
    None
}

pub(crate) fn format_openai_tool_calls(tool_calls: Option<&Value>) -> Option<String> {
    let items = tool_calls?.as_array()?;
    let mut parts = Vec::new();
    for tc in items {
        let name = tc
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("tool");
        let args = tc
            .get("function")
            .and_then(|f| f.get("arguments"))
            .and_then(|a| a.as_str())
            .unwrap_or("{}");
        parts.push(format!("```tool:{name}\n{args}\n```"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_sse_tool_parser_emits_tool_snapshot() {
        let raw = include_str!("../../../tests/fixtures/local/response/completions/stream_tool_call.txt");
        let mut parser = CompletionsSseToolParser::default();
        let snap = parser.push_chunk(raw).expect("tool snapshot");
        assert!(snap.contains("```tool:shell"));
        assert!(snap.contains("ls"));
    }

    #[test]
    fn completions_sse_extracts_tool_calls_without_trailing_newline() {
        let raw = include_str!("../../../tests/fixtures/local/response/completions/stream_tool_call.txt");
        let trimmed = raw.trim_end();
        let out = completions_sse_turn(trimmed);
        assert!(out.contains("```tool:shell"));
        assert!(out.contains("\"command\": \"ls\"") || out.contains("\"command\":\"ls\""));
    }

    #[test]
    fn completions_sse_extracts_multiple_tool_calls() {
        let sse = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"read_file","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\": \"a.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"name":"read_file","arguments":""}}]}}]}

data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\": \"b.rs\"}"}}]}}]}

data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}
"#;
        let out = completions_sse_turn(sse);
        assert!(out.contains("```tool:read_file"));
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
    }

    #[test]
    fn openai_completions_sse_extracts_assistant_text() {
        let raw = include_str!("../../../tests/fixtures/local/response/completions/stream_head.txt");
        let out = completions_sse_turn(raw);
        assert!(out.contains("Hi"));
        assert!(out.contains("help"));
    }

    #[test]
    fn openai_completions_sse_extracts_tool_calls() {
        let raw = include_str!("../../../tests/fixtures/local/response/completions/stream_tool_call.txt");
        let out = completions_sse_turn(raw);
        assert!(out.contains("```tool:shell"));
        assert!(out.contains("ls"));
    }
}
