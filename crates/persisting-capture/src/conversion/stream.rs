//! OpenAI chat completion SSE → Anthropic messages SSE (text deltas).

use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::usage::{StreamMetrics, TokenUsage};

/// Incremental translator for one upstream OpenAI SSE chunk.
pub struct CompletionsStreamTranslator {
    client_model: String,
    message_id: String,
    started: Instant,
    started_events: bool,
    block_open: bool,
    metrics: StreamMetrics,
    upstream_buf: String,
}

impl CompletionsStreamTranslator {
    pub fn new(client_model: impl Into<String>) -> Self {
        Self {
            client_model: client_model.into(),
            message_id: format!("msg_{}", chrono::Utc::now().timestamp_millis()),
            started: Instant::now(),
            started_events: false,
            block_open: false,
            metrics: StreamMetrics::default(),
            upstream_buf: String::new(),
        }
    }

    pub fn metrics(&self) -> &StreamMetrics {
        &self.metrics
    }

    pub fn upstream_snapshot(&self) -> &str {
        &self.upstream_buf
    }

    /// Feed raw upstream bytes; returns Anthropic SSE wire text for the client.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<String> {
        self.upstream_buf.push_str(&String::from_utf8_lossy(chunk));
        let mut out = String::new();
        while let Some(line) = next_sse_data_line(&mut self.upstream_buf) {
            if line == "[DONE]" {
                out.push_str(&self.finish()?);
                continue;
            }
            let v: Value = serde_json::from_str(&line).context("parse OpenAI stream chunk")?;
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
                if !self.started_events {
                    out.push_str(&format_message_start(&self.message_id, &self.client_model));
                    out.push_str("event: content_block_start\n");
                    out.push_str(
                        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
                    );
                    self.started_events = true;
                    self.block_open = true;
                }
                let delta = json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": text},
                });
                out.push_str("event: content_block_delta\n");
                out.push_str(&format!("data: {}\n\n", delta));
            }
            if let Some(reason) = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|f| f.as_str())
            {
                if reason == "stop" || reason == "length" {
                    out.push_str(&self.finish()?);
                }
            }
        }
        Ok(out)
    }

    /// Emit terminal Anthropic events when upstream closes.
    pub fn finish_stream(&mut self) -> Result<String> {
        self.finish()
    }

    fn finish(&mut self) -> Result<String> {
        if !self.started_events {
            return Ok(String::new());
        }
        let mut out = String::new();
        if self.block_open {
            out.push_str("event: content_block_stop\n");
            out.push_str("data: {\"type\":\"content_block_stop\",\"index\":0}\n\n");
            self.block_open = false;
        }
        let usage = json!({
            "input_tokens": self.metrics.usage.input_tokens,
            "output_tokens": self.metrics.usage.output_tokens,
            "cache_read_input_tokens": self.metrics.usage.cache_read_tokens,
        });
        let delta = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": usage,
        });
        out.push_str("event: message_delta\n");
        out.push_str(&format!("data: {}\n\n", delta));
        out.push_str("event: message_stop\n");
        out.push_str("data: {\"type\":\"message_stop\"}\n\n");
        Ok(out)
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
    format!("event: message_start\ndata: {}\n\n", data)
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
    if let Some(u) = v.get("usage") {
        return crate::usage::extract_usage_from_response(u);
    }
    TokenUsage::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_translator_emits_anthropic_events() {
        let raw = include_str!("../../tests/fixtures/response/completions/stream_head.txt");
        let out = translate_completions_sse_to_messages(raw, "claude-test").unwrap();
        assert!(out.contains("event: message_start"));
        assert!(out.contains("content_block_delta"));
        assert!(out.contains("text_delta"));
        assert!(out.contains("message_stop"));
    }
}
