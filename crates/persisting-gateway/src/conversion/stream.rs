//! OpenAI chat completion SSE → Anthropic Messages SSE.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::conversion::MAX_SSE_FRAME_BYTES;
use crate::usage::{StreamMetrics, TokenUsage};

/// Incremental translator for one upstream OpenAI SSE chunk.
pub struct CompletionsStreamTranslator {
    client_model: String,
    message_id: String,
    started: Instant,
    message_started: bool,
    text_block_index: Option<u32>,
    next_block_index: u32,
    tool_calls: BTreeMap<u32, ToolCallState>,
    stop_reason: Option<&'static str>,
    cache_usage_seen: bool,
    finished: bool,
    metrics: StreamMetrics,
    upstream_buf: String,
    upstream_raw: String,
    accumulated_text: String,
}

#[derive(Default)]
struct ToolCallState {
    block_index: u32,
    id: String,
    name: String,
    started: bool,
    stopped: bool,
}

impl CompletionsStreamTranslator {
    pub fn new(client_model: impl Into<String>) -> Self {
        Self {
            client_model: client_model.into(),
            message_id: format!("msg_{}", chrono::Utc::now().timestamp_millis()),
            started: Instant::now(),
            message_started: false,
            text_block_index: None,
            next_block_index: 0,
            tool_calls: BTreeMap::new(),
            stop_reason: None,
            cache_usage_seen: false,
            finished: false,
            metrics: StreamMetrics::default(),
            upstream_buf: String::new(),
            upstream_raw: String::new(),
            accumulated_text: String::new(),
        }
    }

    pub fn metrics(&self) -> &StreamMetrics {
        &self.metrics
    }

    pub fn upstream_snapshot(&self) -> &str {
        &self.upstream_raw
    }

    pub fn accumulated_assistant_text(&self) -> &str {
        &self.accumulated_text
    }

    /// Feed raw upstream bytes; returns Anthropic SSE wire text for the client.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<String> {
        let chunk_str = String::from_utf8_lossy(chunk);
        self.upstream_raw.push_str(&chunk_str);
        self.upstream_buf.push_str(&chunk_str);
        let mut out = String::new();
        while let Some(line) = next_sse_data_line(&mut self.upstream_buf) {
            if line == "[DONE]" {
                out.push_str(&self.finish()?);
                continue;
            }
            let v: Value = serde_json::from_str(&line).context("parse OpenAI stream chunk")?;
            if !self.message_started {
                if let Some(id) = v
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    self.message_id = id.to_string();
                }
            }
            self.cache_usage_seen |= v
                .get("usage")
                .and_then(|usage| usage.get("prompt_tokens_details"))
                .is_some_and(Value::is_object);
            self.metrics
                .usage
                .merge(&extract_usage_from_response_chunk(&v));
            if let Some(text) = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
                .filter(|s| !s.is_empty())
            {
                if self.metrics.ttft_ms.is_none() {
                    self.metrics.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
                }
                self.accumulated_text.push_str(text);
                out.push_str(&self.ensure_message_started());
                let index = match self.text_block_index {
                    Some(index) => index,
                    None => {
                        let index = self.allocate_block_index();
                        self.text_block_index = Some(index);
                        out.push_str(&format_event(
                            "content_block_start",
                            &json!({
                                "type": "content_block_start",
                                "index": index,
                                "content_block": {"type": "text", "text": ""},
                            }),
                        ));
                        index
                    }
                };
                let delta = json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": text},
                });
                out.push_str(&format_event("content_block_delta", &delta));
            }
            if let Some(tool_calls) = v
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("delta"))
                .and_then(|delta| delta.get("tool_calls"))
                .and_then(Value::as_array)
            {
                for tool_call in tool_calls {
                    out.push_str(&self.handle_tool_call_delta(tool_call));
                }
            }
            if let Some(reason) = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|f| f.as_str())
            {
                self.stop_reason = Some(openai_finish_to_anthropic(reason));
            }
        }
        anyhow::ensure!(
            self.upstream_buf.len() <= MAX_SSE_FRAME_BYTES,
            "OpenAI SSE frame exceeds {MAX_SSE_FRAME_BYTES} bytes"
        );
        Ok(out)
    }

    /// Emit terminal Anthropic events when upstream closes.
    pub fn finish_stream(&mut self) -> Result<String> {
        self.finish()
    }

    fn finish(&mut self) -> Result<String> {
        if self.finished {
            return Ok(String::new());
        }
        self.finished = true;
        let mut out = String::new();
        out.push_str(&self.ensure_message_started());
        if let Some(index) = self.text_block_index.take() {
            out.push_str(&format_event(
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": index}),
            ));
        }
        for state in self.tool_calls.values_mut() {
            if state.started && !state.stopped {
                out.push_str(&format_event(
                    "content_block_stop",
                    &json!({"type": "content_block_stop", "index": state.block_index}),
                ));
                state.stopped = true;
            }
        }
        let mut usage = json!({
            "input_tokens": self.metrics.usage.input_tokens
                .saturating_sub(self.metrics.usage.cache_read_tokens)
                .saturating_sub(self.metrics.usage.cache_write_tokens),
            "output_tokens": self.metrics.usage.output_tokens,
        });
        if self.metrics.usage.cache_write_tokens > 0 {
            usage["cache_creation_input_tokens"] = json!(self.metrics.usage.cache_write_tokens);
        }
        if self.cache_usage_seen || self.metrics.usage.cache_read_tokens > 0 {
            usage["cache_read_input_tokens"] = json!(self.metrics.usage.cache_read_tokens);
        }
        let delta = json!({
            "type": "message_delta",
            "delta": {"stop_reason": self.stop_reason.unwrap_or("end_turn"), "stop_sequence": null},
            "usage": usage,
        });
        out.push_str(&format_event("message_delta", &delta));
        out.push_str(&format_event(
            "message_stop",
            &json!({"type": "message_stop"}),
        ));
        Ok(out)
    }

    fn ensure_message_started(&mut self) -> String {
        if self.message_started {
            return String::new();
        }
        self.message_started = true;
        format_message_start(&self.message_id, &self.client_model)
    }

    fn allocate_block_index(&mut self) -> u32 {
        let index = self.next_block_index;
        self.next_block_index += 1;
        index
    }

    fn handle_tool_call_delta(&mut self, tool_call: &Value) -> String {
        let tool_index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        if !self.tool_calls.contains_key(&tool_index) {
            let block_index = self.allocate_block_index();
            self.tool_calls.insert(
                tool_index,
                ToolCallState {
                    block_index,
                    ..ToolCallState::default()
                },
            );
        }

        let state = self
            .tool_calls
            .get_mut(&tool_index)
            .expect("inserted above");
        if let Some(id) = tool_call
            .get("id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            state.id = id.to_string();
        }
        if let Some(name) = tool_call
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            state.name = name.to_string();
        }
        let arguments = tool_call
            .get("function")
            .and_then(|function| function.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or("");

        let mut out = String::new();
        if !state.started {
            state.started = true;
            out.push_str(&format_event(
                "content_block_start",
                &json!({
                    "type": "content_block_start",
                    "index": state.block_index,
                    "content_block": {
                        "type": "tool_use",
                        "id": if state.id.is_empty() { "call_proxy" } else { &state.id },
                        "name": state.name,
                        "input": {},
                    },
                }),
            ));
        }
        if !arguments.is_empty() {
            out.push_str(&format_event(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": state.block_index,
                    "delta": {"type": "input_json_delta", "partial_json": arguments},
                }),
            ));
        }
        let started = self.ensure_message_started();
        format!("{started}{out}")
    }
}

/// Translate a complete OpenAI SSE buffer to Anthropic SSE (for tests).
pub fn translate_completions_sse_to_messages(raw: &str, client_model: &str) -> Result<String> {
    let mut t = CompletionsStreamTranslator::new(client_model);
    let mut out = t.push_chunk(raw.as_bytes())?;
    out.push_str(&t.finish_stream()?);
    Ok(out)
}

fn format_message_start(id: &str, model: &str) -> String {
    let data = json!({
        "type": "message_start",
        "message": {
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
            }
        }
    });
    format_event("message_start", &data)
}

fn format_event(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn openai_finish_to_anthropic(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

fn next_sse_data_line(buf: &mut String) -> Option<String> {
    loop {
        let pos = buf.find("\n\n")?;
        let frame = buf[..pos].to_string();
        *buf = buf[pos + 2..].to_string();
        for line in frame.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() {
                    return Some(data.to_string());
                }
            }
        }
    }
}

fn extract_usage_from_response_chunk(v: &Value) -> TokenUsage {
    if v.get("usage").is_some_and(Value::is_object) {
        return crate::usage::extract_usage_from_response(v);
    }
    TokenUsage::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_translator_emits_anthropic_events() {
        let raw = include_str!("../../tests/fixtures/local/response/completions/stream_head.txt");
        let out = translate_completions_sse_to_messages(raw, "claude-test").unwrap();
        assert!(out.contains("event: message_start"));
        assert!(out.contains("content_block_delta"));
        assert!(out.contains("text_delta"));
        assert!(out.contains("message_stop"));
    }

    #[test]
    fn stream_translator_preserves_tool_calls() {
        let raw =
            include_str!("../../tests/fixtures/local/response/completions/stream_tool_call.txt");
        let out = translate_completions_sse_to_messages(raw, "claude-test").unwrap();
        assert!(out.contains("\"type\":\"tool_use\""));
        assert!(out.contains("\"id\":\"call_abc123\""));
        assert!(out.contains("\"name\":\"shell\""));
        assert!(out.contains("\"partial_json\":\"{\\\"command\\\"\""));
        assert!(out.contains("\"partial_json\":\":\\\"ls\\\"}\""));
        assert!(out.contains("\"stop_reason\":\"tool_use\""));
    }
}
