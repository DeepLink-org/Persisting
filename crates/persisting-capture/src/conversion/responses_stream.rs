//! OpenAI chat completion SSE → OpenAI Responses SSE (text + function_call deltas).

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::tool_call::decode_stream_arguments_delta;
use crate::usage::{StreamMetrics, TokenUsage};

/// Incremental translator: upstream OpenAI completions SSE → client Responses SSE.
pub struct CompletionsToResponsesStreamTranslator {
    client_model: String,
    response_id: String,
    message_item_id: String,
    started: Instant,
    sequence_number: u64,
    sent_created: bool,
    sent_output_item: bool,
    sent_content_part: bool,
    finished: bool,
    metrics: StreamMetrics,
    /// Parsed SSE buffer (consumed line-by-line).
    upstream_buf: String,
    /// Full upstream SSE retained for capture / replay.
    upstream_raw: String,
    accumulated_text: String,
    tool_calls: HashMap<u32, ToolCallState>,
    next_output_index: u32,
    accumulated_reasoning: String,
    pending_tool_call_ids: Vec<String>,
}

struct ToolCallState {
    sse_item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    output_index: u32,
    sent_item_added: bool,
    finalized: bool,
}

impl CompletionsToResponsesStreamTranslator {
    pub fn new(client_model: impl Into<String>) -> Self {
        let ts = chrono::Utc::now().timestamp_millis();
        Self {
            client_model: client_model.into(),
            response_id: format!("resp_{ts}"),
            message_item_id: format!("msg_{ts}"),
            started: Instant::now(),
            sequence_number: 0,
            sent_created: false,
            sent_output_item: false,
            sent_content_part: false,
            finished: false,
            metrics: StreamMetrics::default(),
            upstream_buf: String::new(),
            upstream_raw: String::new(),
            accumulated_text: String::new(),
            tool_calls: HashMap::new(),
            next_output_index: 0,
            accumulated_reasoning: String::new(),
            pending_tool_call_ids: Vec::new(),
        }
    }

    pub fn metrics(&self) -> &StreamMetrics {
        &self.metrics
    }

    /// Tool call ids + reasoning text to cache for DeepSeek follow-up requests.
    pub fn drain_reasoning_snapshot(&mut self) -> (Vec<String>, String) {
        let mut ids: Vec<String> = self
            .tool_calls
            .values()
            .map(|e| e.call_id.clone())
            .filter(|id| !id.is_empty() && id != "call_proxy")
            .collect();
        ids.sort();
        ids.dedup();
        if ids.is_empty() {
            ids = std::mem::take(&mut self.pending_tool_call_ids);
        }
        (ids, std::mem::take(&mut self.accumulated_reasoning))
    }

    pub fn upstream_snapshot(&self) -> &str {
        &self.upstream_raw
    }

    pub fn accumulated_assistant_text(&self) -> &str {
        &self.accumulated_text
    }

    /// Visible assistant narrative for live markdown / `assistant_content` (no tool fences).
    ///
    /// Tool calls remain in Lance via full SSE `body`. Some Codex / reasoning models stream the
    /// user-visible answer in `reasoning_content` while `content` stays empty during tool turns.
    pub fn narrative_for_capture(&self) -> String {
        if !self.accumulated_text.trim().is_empty() {
            return self.accumulated_text.clone();
        }
        if !self.accumulated_reasoning.trim().is_empty() {
            return self.accumulated_reasoning.clone();
        }
        String::new()
    }

    /// Text + in-flight tool calls (debug / spawn-link enrichment when tools appear in content).
    pub fn capture_snapshot(&self) -> String {
        let mut parts = Vec::new();
        let narrative = self.narrative_for_capture();
        if !narrative.is_empty() {
            parts.push(narrative);
        }
        let mut tool_entries: Vec<_> = self.tool_calls.values().collect();
        tool_entries.sort_by_key(|e| e.output_index);
        for entry in tool_entries {
            if entry.name.is_empty() && entry.arguments.is_empty() {
                continue;
            }
            let name = if entry.name.is_empty() {
                "tool"
            } else {
                entry.name.as_str()
            };
            let args = if entry.arguments.trim().is_empty() {
                "{}"
            } else {
                entry.arguments.as_str()
            };
            parts.push(format!("```tool:{name}\n{args}\n```"));
        }
        parts.join("\n\n")
    }

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
            self.metrics
                .usage
                .merge(&extract_usage_from_response_chunk(&v));
            if let Some(model) = v.get("model").and_then(|m| m.as_str()) {
                if !model.is_empty() {
                    self.client_model = model.to_string();
                }
            }
            if !self.sent_created {
                out.push_str(&self.emit_created()?);
            }
            out.push_str(&self.handle_choice_delta(&v)?);
            if let Some(reason) = v
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|f| f.as_str())
            {
                if reason == "stop" || reason == "length" || reason == "tool_calls" {
                    out.push_str(&self.finish()?);
                }
            }
        }
        Ok(out)
    }

    fn handle_choice_delta(&mut self, v: &Value) -> Result<String> {
        let mut out = String::new();
        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            return Ok(out);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);

        if let Some(reasoning) = delta
            .get("reasoning_content")
            .and_then(|r| r.as_str())
            .filter(|s| !s.is_empty())
        {
            self.accumulated_reasoning.push_str(reasoning);
        }

        if let Some(text) = delta
            .get("content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            if self.metrics.ttft_ms.is_none() {
                self.metrics.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
            }
            out.push_str(&self.emit_text_delta(text)?);
            self.accumulated_text.push_str(text);
        }

        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let tool_index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                if !self.tool_calls.contains_key(&tool_index) {
                    let output_index = self.next_output_index;
                    self.next_output_index += 1;
                    let call_id = tc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("call_proxy")
                        .to_string();
                    self.tool_calls.insert(
                        tool_index,
                        ToolCallState {
                            sse_item_id: format!("fc_item_{output_index}"),
                            call_id,
                            name: String::new(),
                            arguments: String::new(),
                            output_index,
                            sent_item_added: false,
                            finalized: false,
                        },
                    );
                }
                {
                    let entry = self.tool_calls.get_mut(&tool_index).unwrap();
                    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                        if !id.is_empty() {
                            entry.call_id = id.to_string();
                            if !self.pending_tool_call_ids.contains(&entry.call_id) {
                                self.pending_tool_call_ids.push(entry.call_id.clone());
                            }
                        }
                    }
                    if let Some(name) = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
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

                let emit = {
                    let entry = self.tool_calls.get(&tool_index).unwrap();
                    let arg_delta = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .filter(|a| !a.is_empty())
                        .map(decode_stream_arguments_delta);
                    let has_identity = (!entry.call_id.is_empty() && entry.call_id != "call_proxy")
                        || !entry.name.is_empty()
                        || tc.get("id").and_then(|i| i.as_str()).is_some()
                        || tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .is_some();
                    (
                        entry.sse_item_id.clone(),
                        entry.call_id.clone(),
                        entry.name.clone(),
                        entry.output_index,
                        entry.sent_item_added,
                        has_identity,
                        arg_delta,
                    )
                };

                if emit.6.is_some() || emit.5 {
                    if self.metrics.ttft_ms.is_none() {
                        self.metrics.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
                    }
                }
                if !emit.4 && emit.5 {
                    out.push_str(
                        &self.emit_function_call_item_added(&emit.0, &emit.1, &emit.2, emit.3)?,
                    );
                    self.tool_calls
                        .get_mut(&tool_index)
                        .unwrap()
                        .sent_item_added = true;
                }
                if let Some(args) = emit.6 {
                    if !self.tool_calls.get(&tool_index).unwrap().sent_item_added {
                        out.push_str(
                            &self
                                .emit_function_call_item_added(&emit.0, &emit.1, &emit.2, emit.3)?,
                        );
                        self.tool_calls
                            .get_mut(&tool_index)
                            .unwrap()
                            .sent_item_added = true;
                    }
                    out.push_str(&self.emit_function_call_arguments_delta(&emit.0, emit.3, &args)?);
                }
            }
        }
        Ok(out)
    }

    pub fn finish_stream(&mut self) -> Result<String> {
        self.finish()
    }

    fn emit_created(&mut self) -> Result<String> {
        self.sent_created = true;
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let created = json!({
            "type": "response.created",
            "sequence_number": seq,
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "in_progress",
                "model": self.client_model,
                "output": []
            }
        });
        Ok(format_sse_event("response.created", &created))
    }

    fn emit_output_item_added(&mut self) -> Result<String> {
        self.sent_output_item = true;
        if self.next_output_index == 0 {
            self.next_output_index = 1;
        }
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.output_item.added",
            "sequence_number": seq,
            "output_index": 0,
            "item": {
                "type": "message",
                "id": self.message_item_id,
                "role": "assistant",
                "status": "in_progress",
                "content": []
            }
        });
        Ok(format_sse_event("response.output_item.added", &evt))
    }

    fn emit_content_part_added(&mut self) -> Result<String> {
        self.sent_content_part = true;
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.content_part.added",
            "sequence_number": seq,
            "item_id": self.message_item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": "",
                "annotations": []
            }
        });
        Ok(format_sse_event("response.content_part.added", &evt))
    }

    fn emit_text_delta(&mut self, delta: &str) -> Result<String> {
        let mut out = String::new();
        if !self.sent_output_item {
            out.push_str(&self.emit_output_item_added()?);
        }
        if !self.sent_content_part {
            out.push_str(&self.emit_content_part_added()?);
        }
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.output_text.delta",
            "sequence_number": seq,
            "item_id": self.message_item_id,
            "output_index": 0,
            "content_index": 0,
            "delta": delta
        });
        out.push_str(&format_sse_event("response.output_text.delta", &evt));
        Ok(out)
    }

    fn emit_function_call_item_added(
        &mut self,
        sse_item_id: &str,
        call_id: &str,
        name: &str,
        output_index: u32,
    ) -> Result<String> {
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.output_item.added",
            "sequence_number": seq,
            "output_index": output_index,
            "item": {
                "type": "function_call",
                "id": call_id,
                "call_id": call_id,
                "name": name,
                "arguments": "",
                "status": "in_progress"
            }
        });
        let _ = sse_item_id;
        Ok(format_sse_event("response.output_item.added", &evt))
    }

    fn emit_function_call_arguments_delta(
        &mut self,
        item_id: &str,
        output_index: u32,
        delta: &str,
    ) -> Result<String> {
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.function_call_arguments.delta",
            "sequence_number": seq,
            "item_id": item_id,
            "output_index": output_index,
            "delta": delta
        });
        Ok(format_sse_event(
            "response.function_call_arguments.delta",
            &evt,
        ))
    }

    fn emit_function_call_arguments_done(&mut self, entry: &ToolCallState) -> Result<String> {
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.function_call_arguments.done",
            "sequence_number": seq,
            "item_id": entry.sse_item_id,
            "output_index": entry.output_index,
            "arguments": entry.arguments
        });
        Ok(format_sse_event(
            "response.function_call_arguments.done",
            &evt,
        ))
    }

    fn emit_output_item_done(&mut self, output_index: u32, item: &Value) -> Result<String> {
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.output_item.done",
            "sequence_number": seq,
            "output_index": output_index,
            "item": item
        });
        Ok(format_sse_event("response.output_item.done", &evt))
    }

    fn emit_content_part_done(&mut self) -> Result<String> {
        self.sequence_number += 1;
        let seq = self.sequence_number;
        let evt = json!({
            "type": "response.content_part.done",
            "sequence_number": seq,
            "item_id": self.message_item_id,
            "output_index": 0,
            "content_index": 0,
            "part": {
                "type": "output_text",
                "text": self.accumulated_text,
                "annotations": []
            }
        });
        Ok(format_sse_event("response.content_part.done", &evt))
    }

    fn finish(&mut self) -> Result<String> {
        if self.finished || !self.sent_created {
            return Ok(String::new());
        }
        self.finished = true;
        let mut out = String::new();

        if self.sent_content_part {
            self.sequence_number += 1;
            let seq = self.sequence_number;
            let evt = json!({
                "type": "response.output_text.done",
                "sequence_number": seq,
                "item_id": self.message_item_id,
                "output_index": 0,
                "content_index": 0,
                "text": self.accumulated_text
            });
            out.push_str(&format_sse_event("response.output_text.done", &evt));
            out.push_str(&self.emit_content_part_done()?);
            let message_item = json!({
                "type": "message",
                "id": self.message_item_id,
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": self.accumulated_text,
                    "annotations": []
                }]
            });
            out.push_str(&self.emit_output_item_done(0, &message_item)?);
        }

        let mut tool_entries: Vec<_> = self.tool_calls.drain().collect();
        tool_entries.sort_by_key(|(idx, _)| *idx);
        let mut output = Vec::new();
        if self.sent_output_item && self.sent_content_part {
            output.push(json!({
                "type": "message",
                "id": self.message_item_id,
                "role": "assistant",
                "status": "completed",
                "content": [{
                    "type": "output_text",
                    "text": self.accumulated_text,
                    "annotations": []
                }]
            }));
        }
        for (_, entry) in &mut tool_entries {
            if entry.sent_item_added && !entry.finalized {
                out.push_str(&self.emit_function_call_arguments_done(entry)?);
                let item = json!({
                    "type": "function_call",
                    "id": entry.call_id,
                    "call_id": entry.call_id,
                    "name": entry.name,
                    "arguments": entry.arguments,
                    "status": "completed"
                });
                out.push_str(&self.emit_output_item_done(entry.output_index, &item)?);
                entry.finalized = true;
            }
            output.push(json!({
                "type": "function_call",
                "id": entry.call_id,
                "call_id": entry.call_id,
                "name": entry.name,
                "arguments": entry.arguments,
                "status": "completed"
            }));
        }

        self.sequence_number += 1;
        let seq = self.sequence_number;
        let usage_evt = json!({
            "type": "response.completed",
            "sequence_number": seq,
            "response": {
                "id": self.response_id,
                "object": "response",
                "status": "completed",
                "model": self.client_model,
                "output": output,
                "usage": {
                    "input_tokens": self.metrics.usage.input_tokens,
                    "output_tokens": self.metrics.usage.output_tokens,
                    "total_tokens": self.metrics.usage.total_tokens
                }
            }
        });
        out.push_str(&format_sse_event("response.completed", &usage_evt));
        Ok(out)
    }
}

fn format_sse_event(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {}\n\n", data)
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

/// Translate a complete OpenAI SSE buffer to Responses SSE (for tests).
pub fn translate_completions_sse_to_responses(raw: &str, client_model: &str) -> Result<String> {
    let mut t = CompletionsToResponsesStreamTranslator::new(client_model);
    let mut out = t.push_chunk(raw.as_bytes())?;
    out.push_str(&t.finish_stream()?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_translator_emits_responses_events() {
        let raw = include_str!("../../tests/fixtures/local/response/completions/stream_head.txt");
        let out = translate_completions_sse_to_responses(raw, "gpt-5.5").unwrap();
        assert!(out.contains("event: response.created"));
        assert!(out.contains("response.output_item.added"));
        assert!(out.contains("response.output_text.delta"));
        assert!(out.contains("response.completed"));
        assert!(out.contains("Hi"));
    }

    #[test]
    fn narrative_for_capture_prefers_content_then_reasoning() {
        let mut t = CompletionsToResponsesStreamTranslator::new("gpt-5.5");
        t.accumulated_text.push_str("visible answer");
        t.accumulated_reasoning.push_str("hidden chain");
        assert_eq!(t.narrative_for_capture(), "visible answer");

        let mut t2 = CompletionsToResponsesStreamTranslator::new("gpt-5.5");
        t2.accumulated_reasoning.push_str("reasoning-only narrative");
        t2.tool_calls.insert(
            0,
            ToolCallState {
                sse_item_id: "fc_0".into(),
                call_id: "call_x".into(),
                name: "exec_command".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
                output_index: 1,
                sent_item_added: true,
                finalized: false,
            },
        );
        assert_eq!(t2.narrative_for_capture(), "reasoning-only narrative");
        assert!(t2.capture_snapshot().contains("```tool:exec_command"));
        assert!(!t2.narrative_for_capture().contains("```tool:"));
    }

    #[test]
    fn stream_translator_emits_function_call_events() {
        let raw =
            include_str!("../../tests/fixtures/local/response/completions/stream_tool_call.txt");
        let out = translate_completions_sse_to_responses(raw, "gpt-5.5").unwrap();
        assert!(out.contains("response.output_item.added"));
        assert!(out.contains("response.function_call_arguments.delta"));
        assert!(out.contains("response.function_call_arguments.done"));
        assert!(out.contains("response.output_item.done"));
        assert!(out.contains("fc_item_"));
        assert!(out.contains("function_call"));
        assert!(out.contains("shell"));
    }
}
