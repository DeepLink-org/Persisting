//! Provider SSE → Chronicle stream events → client SSE.
//!
//! Decoding and rendering are deliberately separate. No client/provider wire
//! shape is used as an intermediate protocol.

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use anyhow::{Context, Result};
use bytes::Bytes;
use persisting_pchronicle::{
    LlmCandidate, LlmContentPart, LlmMessage, LlmProtocol, LlmResponse, LlmResponseEventPayload,
    LlmRole, LlmStreamEvent, LlmUsage, LLM_EVENT_SCHEMA_VERSION,
};
use serde_json::{json, Value};

use super::{ProtocolBridge, MAX_SSE_FRAME_BYTES};
use crate::protocol::ProtocolKind;
use crate::usage::{extract_usage_from_response, StreamMetrics, TokenUsage};

pub struct TypedStreamTranslator {
    passthrough: bool,
    decoder: StreamDecoder,
    renderer: StreamRenderer,
}

impl TypedStreamTranslator {
    pub fn new(bridge: ProtocolBridge, client: ProtocolKind, client_model: &str) -> Option<Self> {
        let upstream = bridge.upstream_protocol(client);
        let upstream = llm_protocol(upstream)?;
        let target = llm_protocol(client)?;
        Some(Self {
            passthrough: bridge == ProtocolBridge::Passthrough,
            decoder: StreamDecoder::new(upstream, client_model),
            renderer: StreamRenderer::new(target, client_model),
        })
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<Bytes> {
        if self.passthrough {
            // Semantic capture is best-effort on an otherwise transparent route.
            // Provider extensions or malformed frames must never alter or block
            // the exact upstream bytes returned to the client.
            if let Err(error) = self.decoder.push_chunk(chunk) {
                tracing::warn!(
                    target: "persisting_gateway",
                    "passthrough stream semantic decode: {error:#}"
                );
            }
            return Ok(Bytes::copy_from_slice(chunk));
        }
        let events = self.decoder.push_chunk(chunk)?;
        let rendered = self.renderer.push_events(&events)?;
        Ok(Bytes::from(rendered))
    }

    pub fn finish_stream(&mut self) -> Result<Bytes> {
        if self.passthrough {
            if let Err(error) = self.decoder.finish() {
                tracing::warn!(
                    target: "persisting_gateway",
                    "passthrough stream semantic finish: {error:#}"
                );
            }
            return Ok(Bytes::new());
        }
        let events = self.decoder.finish()?;
        let rendered = self.renderer.finish(&events)?;
        Ok(Bytes::from(rendered))
    }

    pub fn metrics(&self) -> &StreamMetrics {
        self.decoder.metrics()
    }

    pub fn upstream_snapshot(&self) -> &[u8] {
        self.decoder.upstream_snapshot()
    }

    pub fn streaming_capture_snapshot(&self) -> Option<String> {
        let text = self.decoder.accumulator.visible_text();
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn semantic_response(&self) -> LlmResponseEventPayload {
        self.decoder.semantic_response()
    }

    pub fn drain_reasoning_snapshot(&mut self) -> (Vec<String>, String) {
        self.decoder.accumulator.drain_reasoning_snapshot()
    }
}

fn llm_protocol(protocol: ProtocolKind) -> Option<LlmProtocol> {
    match protocol {
        ProtocolKind::ChatCompletions => Some(LlmProtocol::ChatCompletions),
        ProtocolKind::Messages => Some(LlmProtocol::Messages),
        ProtocolKind::Responses => Some(LlmProtocol::Responses),
        ProtocolKind::Gemini => Some(LlmProtocol::Gemini),
        _ => None,
    }
}

struct StreamDecoder {
    protocol: LlmProtocol,
    fallback_model: String,
    buffer: Vec<u8>,
    upstream_raw: Vec<u8>,
    started: Instant,
    emitted_start: bool,
    finished: bool,
    metrics: StreamMetrics,
    tool_ids: HashMap<(usize, usize), String>,
    accumulator: ResponseAccumulator,
}

impl StreamDecoder {
    fn new(protocol: LlmProtocol, fallback_model: &str) -> Self {
        Self {
            protocol: protocol.clone(),
            fallback_model: fallback_model.into(),
            buffer: Vec::new(),
            upstream_raw: Vec::new(),
            started: Instant::now(),
            emitted_start: false,
            finished: false,
            metrics: StreamMetrics::default(),
            tool_ids: HashMap::new(),
            accumulator: ResponseAccumulator::new(protocol),
        }
    }

    fn push_chunk(&mut self, chunk: &[u8]) -> Result<Vec<LlmStreamEvent>> {
        self.upstream_raw.extend_from_slice(chunk);
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(frame) = next_sse_frame(&mut self.buffer) {
            let frame = std::str::from_utf8(&frame).context("provider SSE frame is not UTF-8")?;
            let data = sse_frame_data(frame);
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(&data).context("parse provider SSE data")?;
            match self.protocol {
                LlmProtocol::Gemini => self.decode_gemini(&value, &mut events),
                LlmProtocol::ChatCompletions => self.decode_chat(&value, &mut events),
                LlmProtocol::Messages => self.decode_messages(&value, &mut events),
                LlmProtocol::Responses => self.decode_responses(&value, &mut events),
                LlmProtocol::Unknown => {}
            }
        }
        anyhow::ensure!(
            self.buffer.len() <= MAX_SSE_FRAME_BYTES,
            "provider SSE frame exceeds {MAX_SSE_FRAME_BYTES} bytes"
        );
        self.observe(&events);
        Ok(events)
    }

    fn finish(&mut self) -> Result<Vec<LlmStreamEvent>> {
        if self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        Ok(Vec::new())
    }

    fn decode_chat(&mut self, value: &Value, events: &mut Vec<LlmStreamEvent>) {
        self.start_from(
            value.get("id").and_then(Value::as_str),
            value.get("model").and_then(Value::as_str),
            events,
        );
        for (position, choice) in value
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let candidate = choice
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or(position);
            let delta = choice.get("delta").unwrap_or(choice);
            if let Some(text) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                events.push(LlmStreamEvent::TextDelta {
                    candidate,
                    text: text.into(),
                });
            }
            if let Some(text) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                events.push(LlmStreamEvent::ReasoningDelta {
                    candidate,
                    text: text.into(),
                });
            }
            for (tool_position, tool) in delta
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let index = tool
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(tool_position);
                let function = tool.get("function").unwrap_or(tool);
                let id = tool
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .or_else(|| self.tool_ids.get(&(candidate, index)).cloned())
                    .unwrap_or_else(|| format!("call_stream_{candidate}_{index}"));
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.tool_ids.entry((candidate, index))
                {
                    entry.insert(id.clone());
                    events.push(LlmStreamEvent::ToolCallStart {
                        candidate,
                        id: id.clone(),
                        name: name.into(),
                        signature: None,
                    });
                }
                if let Some(delta) = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .filter(|delta| !delta.is_empty())
                {
                    events.push(LlmStreamEvent::ToolArgumentsDelta {
                        candidate,
                        id,
                        delta: super::decode_stream_arguments_delta(delta),
                    });
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                events.push(LlmStreamEvent::Finish {
                    candidate,
                    reason: Some(reason.into()),
                });
            }
        }
        self.usage_from(value, events);
    }

    fn decode_gemini(&mut self, value: &Value, events: &mut Vec<LlmStreamEvent>) {
        self.start_from(
            value.get("responseId").and_then(Value::as_str),
            value.get("modelVersion").and_then(Value::as_str),
            events,
        );
        for (position, candidate_value) in value
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let candidate = candidate_value
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .unwrap_or(position);
            for (part_index, part) in candidate_value
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        events.push(LlmStreamEvent::ReasoningDelta {
                            candidate,
                            text: text.into(),
                        });
                    } else {
                        events.push(LlmStreamEvent::TextDelta {
                            candidate,
                            text: text.into(),
                        });
                    }
                }
                if let Some(call) = part.get("functionCall") {
                    let id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_gemini_{candidate}_{part_index}"));
                    events.push(LlmStreamEvent::ToolCallStart {
                        candidate,
                        id: id.clone(),
                        name: call
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        signature: part
                            .get("thoughtSignature")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                    events.push(LlmStreamEvent::ToolArgumentsDelta {
                        candidate,
                        id,
                        delta: serde_json::to_string(call.get("args").unwrap_or(&json!({})))
                            .unwrap_or_else(|_| "{}".into()),
                    });
                }
            }
            if let Some(reason) = candidate_value.get("finishReason").and_then(Value::as_str) {
                events.push(LlmStreamEvent::Finish {
                    candidate,
                    reason: Some(reason.into()),
                });
            }
        }
        self.usage_from(value, events);
    }

    fn decode_messages(&mut self, value: &Value, events: &mut Vec<LlmStreamEvent>) {
        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                let message = value.get("message").unwrap_or(value);
                self.start_from(
                    message.get("id").and_then(Value::as_str),
                    message.get("model").and_then(Value::as_str),
                    events,
                );
                self.usage_from(message, events);
            }
            Some("content_block_start") => {
                let block = value.get("content_block").unwrap_or(value);
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    let block_index = value
                        .get("index")
                        .and_then(Value::as_u64)
                        .and_then(|index| usize::try_from(index).ok())
                        .unwrap_or(0);
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_stream")
                        .to_string();
                    self.tool_ids.insert((0, block_index), id.clone());
                    events.push(LlmStreamEvent::ToolCallStart {
                        candidate: 0,
                        id,
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        signature: None,
                    });
                }
            }
            Some("content_block_delta") => {
                let delta = value.get("delta").unwrap_or(value);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            events.push(LlmStreamEvent::TextDelta {
                                candidate: 0,
                                text: text.into(),
                            });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            events.push(LlmStreamEvent::ReasoningDelta {
                                candidate: 0,
                                text: text.into(),
                            });
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(arguments) = delta.get("partial_json").and_then(Value::as_str) {
                            let block_index = value
                                .get("index")
                                .and_then(Value::as_u64)
                                .and_then(|index| usize::try_from(index).ok())
                                .unwrap_or(0);
                            let id = self
                                .tool_ids
                                .get(&(0, block_index))
                                .cloned()
                                .unwrap_or_else(|| "call_stream".into());
                            events.push(LlmStreamEvent::ToolArgumentsDelta {
                                candidate: 0,
                                id,
                                delta: arguments.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("message_delta") => {
                let delta = value.get("delta").unwrap_or(value);
                if let Some(reason) = delta.get("stop_reason").and_then(Value::as_str) {
                    events.push(LlmStreamEvent::Finish {
                        candidate: 0,
                        reason: Some(reason.into()),
                    });
                }
                self.usage_from(value, events);
            }
            _ => {}
        }
    }

    fn decode_responses(&mut self, value: &Value, events: &mut Vec<LlmStreamEvent>) {
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !self.emitted_start {
            let response = value.get("response").unwrap_or(value);
            self.start_from(
                response.get("id").and_then(Value::as_str),
                response.get("model").and_then(Value::as_str),
                events,
            );
        }
        match event_type {
            "response.output_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    events.push(LlmStreamEvent::TextDelta {
                        candidate: 0,
                        text: text.into(),
                    });
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(text) = value.get("delta").and_then(Value::as_str) {
                    events.push(LlmStreamEvent::ReasoningDelta {
                        candidate: 0,
                        text: text.into(),
                    });
                }
            }
            "response.output_item.added" => {
                let item = value.get("item").unwrap_or(value);
                if matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call" | "custom_tool_call")
                ) {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("call_stream");
                    events.push(LlmStreamEvent::ToolCallStart {
                        candidate: 0,
                        id: id.into(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        signature: None,
                    });
                }
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let id = value
                    .get("call_id")
                    .or_else(|| value.get("item_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("call_stream");
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    events.push(LlmStreamEvent::ToolArgumentsDelta {
                        candidate: 0,
                        id: id.into(),
                        delta: delta.into(),
                    });
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = value.get("response").unwrap_or(value);
                self.usage_from(response, events);
                events.push(LlmStreamEvent::Finish {
                    candidate: 0,
                    reason: response
                        .get("status")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
            _ => {}
        }
    }

    fn start_from(
        &mut self,
        id: Option<&str>,
        model: Option<&str>,
        events: &mut Vec<LlmStreamEvent>,
    ) {
        if self.emitted_start {
            if self.accumulator.model.is_none() {
                self.accumulator.model = model.map(str::to_string);
            }
            return;
        }
        self.emitted_start = true;
        let id = id.map(str::to_string);
        let model = model
            .map(str::to_string)
            .or_else(|| Some(self.fallback_model.clone()));
        events.push(LlmStreamEvent::Start { id, model });
    }

    fn usage_from(&self, value: &Value, events: &mut Vec<LlmStreamEvent>) {
        let usage = extract_usage_from_response(value);
        if usage != TokenUsage::default() {
            events.push(LlmStreamEvent::Usage {
                usage: to_llm_usage(&usage),
            });
        }
    }

    fn observe(&mut self, events: &[LlmStreamEvent]) {
        if self.metrics.ttft_ms.is_none()
            && events.iter().any(|event| {
                matches!(
                    event,
                    LlmStreamEvent::TextDelta { .. }
                        | LlmStreamEvent::ReasoningDelta { .. }
                        | LlmStreamEvent::ToolCallStart { .. }
                )
            })
        {
            self.metrics.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
        }
        for event in events {
            if let LlmStreamEvent::Usage { usage } = event {
                self.metrics.usage = from_llm_usage(usage);
            }
            self.accumulator.apply(event);
        }
    }

    fn metrics(&self) -> &StreamMetrics {
        &self.metrics
    }
    fn upstream_snapshot(&self) -> &[u8] {
        &self.upstream_raw
    }

    fn semantic_response(&self) -> LlmResponseEventPayload {
        self.accumulator.response()
    }
}

#[derive(Default)]
struct CandidateState {
    text: String,
    reasoning: String,
    tools: Vec<ToolState>,
    finish_reason: Option<String>,
}

struct ToolState {
    id: String,
    name: String,
    arguments: String,
    signature: Option<String>,
}

struct ResponseAccumulator {
    protocol: LlmProtocol,
    id: Option<String>,
    model: Option<String>,
    usage: Option<LlmUsage>,
    candidates: BTreeMap<usize, CandidateState>,
}

impl ResponseAccumulator {
    fn new(protocol: LlmProtocol) -> Self {
        Self {
            protocol,
            id: None,
            model: None,
            usage: None,
            candidates: BTreeMap::new(),
        }
    }

    fn apply(&mut self, event: &LlmStreamEvent) {
        match event {
            LlmStreamEvent::Start { id, model } => {
                self.id = id.clone();
                self.model = model.clone();
            }
            LlmStreamEvent::TextDelta { candidate, text } => self
                .candidates
                .entry(*candidate)
                .or_default()
                .text
                .push_str(text),
            LlmStreamEvent::ReasoningDelta { candidate, text } => self
                .candidates
                .entry(*candidate)
                .or_default()
                .reasoning
                .push_str(text),
            LlmStreamEvent::ToolCallStart {
                candidate,
                id,
                name,
                signature,
            } => {
                let state = self.candidates.entry(*candidate).or_default();
                if let Some(tool) = state.tools.iter_mut().find(|tool| tool.id == *id) {
                    tool.name = name.clone();
                    tool.signature = signature.clone();
                } else {
                    state.tools.push(ToolState {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        signature: signature.clone(),
                    });
                }
            }
            LlmStreamEvent::ToolArgumentsDelta {
                candidate,
                id,
                delta,
            } => {
                let state = self.candidates.entry(*candidate).or_default();
                if let Some(tool) = state.tools.iter_mut().find(|tool| tool.id == *id) {
                    tool.arguments.push_str(delta);
                } else {
                    state.tools.push(ToolState {
                        id: id.clone(),
                        name: String::new(),
                        arguments: delta.clone(),
                        signature: None,
                    });
                }
            }
            LlmStreamEvent::Usage { usage } => self.usage = Some(usage.clone()),
            LlmStreamEvent::Finish { candidate, reason } => {
                self.candidates.entry(*candidate).or_default().finish_reason = reason.clone()
            }
            LlmStreamEvent::Error { .. } => {}
        }
    }

    fn visible_text(&self) -> String {
        let text = self
            .candidates
            .values()
            .map(|candidate| candidate.text.as_str())
            .collect::<String>();
        if text.trim().is_empty() {
            self.candidates
                .values()
                .map(|candidate| candidate.reasoning.as_str())
                .collect()
        } else {
            text
        }
    }

    fn drain_reasoning_snapshot(&mut self) -> (Vec<String>, String) {
        let ids = self
            .candidates
            .values()
            .flat_map(|candidate| candidate.tools.iter().map(|tool| tool.id.clone()))
            .collect();
        let reasoning = self
            .candidates
            .values_mut()
            .map(|candidate| std::mem::take(&mut candidate.reasoning))
            .collect();
        (ids, reasoning)
    }

    fn response(&self) -> LlmResponseEventPayload {
        let candidates = self
            .candidates
            .iter()
            .map(|(index, state)| {
                let mut parts = Vec::new();
                if !state.reasoning.is_empty() {
                    parts.push(LlmContentPart::Reasoning {
                        text: Some(state.reasoning.clone()),
                        signature: None,
                    });
                }
                if !state.text.is_empty() {
                    parts.push(LlmContentPart::text(&state.text));
                }
                for tool in &state.tools {
                    let arguments = serde_json::from_str(&tool.arguments)
                        .unwrap_or_else(|_| Value::String(tool.arguments.clone()));
                    parts.push(LlmContentPart::ToolCall {
                        id: tool.id.clone(),
                        name: tool.name.clone(),
                        arguments,
                        signature: tool.signature.clone(),
                    });
                }
                LlmCandidate {
                    index: *index,
                    message: LlmMessage::new(LlmRole::Assistant, parts),
                    finish_reason: state.finish_reason.clone(),
                    extensions: BTreeMap::new(),
                }
            })
            .collect();
        LlmResponseEventPayload {
            schema_version: LLM_EVENT_SCHEMA_VERSION.into(),
            output_format: self.protocol.clone(),
            response: LlmResponse {
                id: self.id.clone(),
                model: self.model.clone(),
                candidates,
                usage: self.usage.clone(),
                extensions: BTreeMap::new(),
            },
        }
    }
}

struct StreamRenderer {
    protocol: LlmProtocol,
    model: String,
    id: String,
    created: i64,
    sequence: u64,
    started: bool,
    finished: bool,
    usage: LlmUsage,
    finish_reason: Option<String>,
    blocks: HashMap<String, usize>,
    tool_names: HashMap<String, String>,
    tool_arguments: HashMap<String, String>,
    accumulated_text: String,
    text_block: Option<usize>,
    reasoning_block: Option<usize>,
    next_block: usize,
}

impl StreamRenderer {
    fn new(protocol: LlmProtocol, model: &str) -> Self {
        let timestamp = chrono::Utc::now().timestamp_millis();
        Self {
            protocol,
            model: model.into(),
            id: format!("stream_{timestamp}"),
            created: timestamp / 1000,
            sequence: 0,
            started: false,
            finished: false,
            usage: LlmUsage::default(),
            finish_reason: None,
            blocks: HashMap::new(),
            tool_names: HashMap::new(),
            tool_arguments: HashMap::new(),
            accumulated_text: String::new(),
            text_block: None,
            reasoning_block: None,
            next_block: 0,
        }
    }

    fn push_events(&mut self, events: &[LlmStreamEvent]) -> Result<String> {
        let mut output = String::new();
        for event in events {
            match self.protocol {
                LlmProtocol::ChatCompletions => self.render_chat(event, &mut output)?,
                LlmProtocol::Messages => self.render_messages(event, &mut output)?,
                LlmProtocol::Responses => self.render_responses(event, &mut output)?,
                LlmProtocol::Gemini => self.render_gemini(event, &mut output)?,
                LlmProtocol::Unknown => {}
            }
        }
        Ok(output)
    }

    fn finish(&mut self, events: &[LlmStreamEvent]) -> Result<String> {
        let mut output = self.push_events(events)?;
        if self.finished {
            return Ok(output);
        }
        self.finished = true;
        match self.protocol {
            LlmProtocol::ChatCompletions => output.push_str("data: [DONE]\n\n"),
            LlmProtocol::Messages => self.finish_messages(&mut output)?,
            LlmProtocol::Responses => self.finish_responses(&mut output)?,
            LlmProtocol::Gemini | LlmProtocol::Unknown => {}
        }
        Ok(output)
    }

    fn render_chat(&mut self, event: &LlmStreamEvent, output: &mut String) -> Result<()> {
        let mut choice = None;
        match event {
            LlmStreamEvent::Start { id, .. } => {
                if let Some(id) = id {
                    self.id = id.clone();
                }
                choice = Some(json!({"index":0,"delta":{"role":"assistant"},"finish_reason":null}));
            }
            LlmStreamEvent::TextDelta { candidate, text } => {
                choice =
                    Some(json!({"index":candidate,"delta":{"content":text},"finish_reason":null}))
            }
            LlmStreamEvent::ReasoningDelta { candidate, text } => {
                choice = Some(
                    json!({"index":candidate,"delta":{"reasoning_content":text},"finish_reason":null}),
                )
            }
            LlmStreamEvent::ToolCallStart {
                candidate,
                id,
                name,
                signature,
            } => {
                let wire_id = signature
                    .as_ref()
                    .map(|signature| format!("{id}__thought__{signature}"))
                    .unwrap_or_else(|| id.clone());
                let index = self.ensure_chat_tool_index(id);
                choice = Some(
                    json!({"index":candidate,"delta":{"tool_calls":[{"index":index,"id":wire_id,"type":"function","function":{"name":name,"arguments":""}}]},"finish_reason":null}),
                );
            }
            LlmStreamEvent::ToolArgumentsDelta {
                candidate,
                id,
                delta,
            } => {
                let index = self.ensure_chat_tool_index(id);
                choice = Some(
                    json!({"index":candidate,"delta":{"tool_calls":[{"index":index,"function":{"arguments":delta}}]},"finish_reason":null}),
                );
            }
            LlmStreamEvent::Usage { usage } => self.usage = usage.clone(),
            LlmStreamEvent::Finish { candidate, reason } => {
                self.finish_reason = reason.clone();
                choice = Some(
                    json!({"index":candidate,"delta":{},"finish_reason":super::semantic::chat_finish_reason(reason.as_deref(), !self.blocks.is_empty())}),
                );
            }
            LlmStreamEvent::Error { message, code } => {
                output.push_str(&format_sse(
                    None,
                    &json!({"error":{"message":message,"code":code}}),
                )?);
                return Ok(());
            }
        }
        let mut value = json!({
            "id":self.id,"object":"chat.completion.chunk","created":self.created,"model":self.model,
            "choices":choice.into_iter().collect::<Vec<_>>(),
        });
        if matches!(event, LlmStreamEvent::Usage { .. }) {
            value["usage"] = super::semantic::render_chat_usage(&self.usage);
        }
        output.push_str(&format_sse(None, &value)?);
        Ok(())
    }

    fn render_messages(&mut self, event: &LlmStreamEvent, output: &mut String) -> Result<()> {
        self.ensure_messages_start(event, output)?;
        match event {
            LlmStreamEvent::TextDelta { text, .. } => {
                self.accumulated_text.push_str(text);
                let index = self.ensure_message_block("text", None, output)?;
                output.push_str(&format_sse(Some("content_block_delta"), &json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}))?);
            }
            LlmStreamEvent::ReasoningDelta { text, .. } => {
                let index = self.ensure_message_block("thinking", None, output)?;
                output.push_str(&format_sse(Some("content_block_delta"), &json!({"type":"content_block_delta","index":index,"delta":{"type":"thinking_delta","thinking":text}}))?);
            }
            LlmStreamEvent::ToolCallStart { id, name, .. } => {
                self.tool_names.insert(id.clone(), name.clone());
                self.ensure_message_block("tool_use", Some((id, name)), output)?;
            }
            LlmStreamEvent::ToolArgumentsDelta { id, delta, .. } => {
                let empty_name = String::new();
                let index =
                    self.ensure_message_block("tool_use", Some((id, &empty_name)), output)?;
                self.tool_arguments
                    .entry(id.clone())
                    .or_default()
                    .push_str(delta);
                output.push_str(&format_sse(Some("content_block_delta"), &json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":delta}}))?);
            }
            LlmStreamEvent::Usage { usage } => self.usage = usage.clone(),
            LlmStreamEvent::Finish { reason, .. } => self.finish_reason = reason.clone(),
            _ => {}
        }
        Ok(())
    }

    fn ensure_messages_start(&mut self, event: &LlmStreamEvent, output: &mut String) -> Result<()> {
        if let LlmStreamEvent::Start { id: Some(id), .. } = event {
            self.id = id.clone();
        }
        if !self.started {
            self.started = true;
            output.push_str(&format_sse(Some("message_start"), &json!({"type":"message_start","message":{"id":self.id,"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":0,"output_tokens":0}}}))?);
        }
        Ok(())
    }

    fn ensure_message_block(
        &mut self,
        kind: &str,
        tool: Option<(&String, &String)>,
        output: &mut String,
    ) -> Result<usize> {
        let existing = match kind {
            "text" => self.text_block,
            "thinking" => self.reasoning_block,
            _ => tool.and_then(|(id, _)| self.blocks.get(id).copied()),
        };
        if let Some(index) = existing {
            return Ok(index);
        }
        let index = self.next_block;
        self.next_block += 1;
        let block = match (kind, tool) {
            ("tool_use", Some((id, name))) => {
                self.blocks.insert(id.clone(), index);
                self.tool_names.insert(id.clone(), name.clone());
                self.tool_arguments.entry(id.clone()).or_default();
                json!({"type":"tool_use","id":id,"name":name,"input":{}})
            }
            ("thinking", _) => {
                self.reasoning_block = Some(index);
                json!({"type":"thinking","thinking":""})
            }
            _ => {
                self.text_block = Some(index);
                json!({"type":"text","text":""})
            }
        };
        output.push_str(&format_sse(
            Some("content_block_start"),
            &json!({"type":"content_block_start","index":index,"content_block":block}),
        )?);
        Ok(index)
    }

    fn ensure_chat_tool_index(&mut self, id: &str) -> usize {
        if let Some(index) = self.blocks.get(id) {
            return *index;
        }
        let index = self.blocks.len();
        self.blocks.insert(id.to_string(), index);
        index
    }

    fn finish_messages(&mut self, output: &mut String) -> Result<()> {
        for index in 0..self.next_block {
            output.push_str(&format_sse(
                Some("content_block_stop"),
                &json!({"type":"content_block_stop","index":index}),
            )?);
        }
        let mut usage = json!({
            "input_tokens": self.usage.input_tokens,
            "output_tokens": self.usage.output_tokens,
            "cache_read_input_tokens": self.usage.cache_read_tokens,
        });
        if self.usage.cache_write_tokens > 0 {
            usage["cache_creation_input_tokens"] = json!(self.usage.cache_write_tokens);
        }
        output.push_str(&format_sse(Some("message_delta"), &json!({"type":"message_delta","delta":{"stop_reason":super::semantic::anthropic_finish_reason(self.finish_reason.as_deref(), !self.blocks.is_empty()),"stop_sequence":null},"usage":usage}))?);
        output.push_str(&format_sse(
            Some("message_stop"),
            &json!({"type":"message_stop"}),
        )?);
        Ok(())
    }

    fn render_responses(&mut self, event: &LlmStreamEvent, output: &mut String) -> Result<()> {
        if let LlmStreamEvent::Start { id: Some(id), .. } = event {
            self.id = format!("resp_{}", super::semantic::safe_id_suffix(id));
        }
        if !self.started {
            self.started = true;
            self.emit_response_event("response.created", json!({"type":"response.created","response":{"id":self.id,"object":"response","status":"in_progress","model":self.model,"output":[]}}), output)?;
        }
        match event {
            LlmStreamEvent::TextDelta { text, .. } => {
                self.accumulated_text.push_str(text);
                if self.text_block.is_none() {
                    let index = self.next_block;
                    self.next_block += 1;
                    self.text_block = Some(index);
                    self.emit_response_event("response.output_item.added", json!({"type":"response.output_item.added","output_index":index,"item":{"type":"message","id":format!("msg_{}",self.id),"role":"assistant","status":"in_progress","content":[]}}), output)?;
                    self.emit_response_event("response.content_part.added", json!({"type":"response.content_part.added","item_id":format!("msg_{}",self.id),"output_index":index,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}), output)?;
                }
                let index = self.text_block.unwrap_or(0);
                self.emit_response_event("response.output_text.delta", json!({"type":"response.output_text.delta","item_id":format!("msg_{}",self.id),"output_index":index,"content_index":0,"delta":text,"logprobs":[]}), output)?;
            }
            LlmStreamEvent::ToolCallStart { id, name, .. } => {
                self.tool_names.insert(id.clone(), name.clone());
                self.tool_arguments.entry(id.clone()).or_default();
                if !self.blocks.contains_key(id) {
                    self.start_response_tool(id, name, output)?;
                }
            }
            LlmStreamEvent::ToolArgumentsDelta { id, delta, .. } => {
                let index = match self.blocks.get(id).copied() {
                    Some(index) => index,
                    None => self.start_response_tool(id, "", output)?,
                };
                self.tool_arguments
                    .entry(id.clone())
                    .or_default()
                    .push_str(delta);
                self.emit_response_event("response.function_call_arguments.delta", json!({"type":"response.function_call_arguments.delta","item_id":format!("fc_{id}"),"output_index":index,"delta":delta}), output)?;
            }
            LlmStreamEvent::ReasoningDelta { .. } => {}
            LlmStreamEvent::Usage { usage } => self.usage = usage.clone(),
            LlmStreamEvent::Finish { reason, .. } => self.finish_reason = reason.clone(),
            _ => {}
        }
        Ok(())
    }

    fn start_response_tool(&mut self, id: &str, name: &str, output: &mut String) -> Result<usize> {
        let index = self.next_block;
        self.next_block += 1;
        self.blocks.insert(id.to_string(), index);
        self.tool_names
            .entry(id.to_string())
            .or_insert_with(|| name.to_string());
        self.tool_arguments.entry(id.to_string()).or_default();
        self.emit_response_event("response.output_item.added", json!({"type":"response.output_item.added","output_index":index,"item":{"type":"function_call","id":format!("fc_{id}"),"call_id":id,"name":name,"arguments":"","status":"in_progress"}}), output)?;
        Ok(index)
    }

    fn emit_response_event(
        &mut self,
        name: &str,
        mut value: Value,
        output: &mut String,
    ) -> Result<()> {
        self.sequence += 1;
        value["sequence_number"] = json!(self.sequence);
        output.push_str(&format_sse(Some(name), &value)?);
        Ok(())
    }

    fn finish_responses(&mut self, output: &mut String) -> Result<()> {
        let mut completed_output = Vec::new();
        if let Some(index) = self.text_block {
            let item_id = format!("msg_{}", self.id);
            let text = self.accumulated_text.clone();
            let item = json!({"type":"message","id":item_id,"role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[],"logprobs":[]}]});
            self.emit_response_event("response.output_text.done", json!({"type":"response.output_text.done","item_id":item_id,"output_index":index,"content_index":0,"text":text,"logprobs":[]}), output)?;
            self.emit_response_event("response.content_part.done", json!({"type":"response.content_part.done","item_id":item_id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":text,"annotations":[]}}), output)?;
            self.emit_response_event(
                "response.output_item.done",
                json!({"type":"response.output_item.done","output_index":index,"item":item}),
                output,
            )?;
            completed_output.push((index, item));
        }
        let mut tools = self
            .blocks
            .iter()
            .map(|(id, index)| {
                (
                    *index,
                    id.clone(),
                    self.tool_names.get(id).cloned().unwrap_or_default(),
                    self.tool_arguments.get(id).cloned().unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>();
        tools.sort_by_key(|(index, ..)| *index);
        for (index, id, name, arguments) in tools {
            let item_id = format!("fc_{id}");
            let item = json!({"type":"function_call","id":item_id,"call_id":id,"name":name,"arguments":arguments,"status":"completed"});
            self.emit_response_event("response.function_call_arguments.done", json!({"type":"response.function_call_arguments.done","item_id":item_id,"output_index":index,"arguments":arguments}), output)?;
            self.emit_response_event(
                "response.output_item.done",
                json!({"type":"response.output_item.done","output_index":index,"item":item}),
                output,
            )?;
            completed_output.push((index, item));
        }
        completed_output.sort_by_key(|(index, _)| *index);
        let completed_output = completed_output
            .into_iter()
            .map(|(_, item)| item)
            .collect::<Vec<_>>();
        let status = match self.finish_reason.as_deref() {
            Some("length" | "max_tokens" | "MAX_TOKENS" | "incomplete") => "incomplete",
            Some("failed" | "content_filter") => "failed",
            _ => "completed",
        };
        let usage = self.usage.clone();
        self.emit_response_event("response.completed", json!({"type":"response.completed","response":{"id":self.id,"object":"response","status":status,"model":self.model,"output":completed_output,"usage":{"input_tokens":usage.input_tokens,"output_tokens":usage.output_tokens,"total_tokens":usage.total_tokens}}}), output)
    }

    fn render_gemini(&mut self, event: &LlmStreamEvent, output: &mut String) -> Result<()> {
        if let LlmStreamEvent::Start { id: Some(id), .. } = event {
            self.id = id.clone();
        }
        let mut parts = Vec::new();
        let mut finish = None;
        match event {
            LlmStreamEvent::TextDelta { text, .. } => parts.push(json!({"text":text})),
            LlmStreamEvent::ReasoningDelta { text, .. } => {
                parts.push(json!({"text":text,"thought":true}))
            }
            LlmStreamEvent::ToolCallStart {
                id,
                name,
                signature,
                ..
            } => {
                let mut p = json!({"functionCall":{"id":id,"name":name,"args":{}}});
                if let Some(signature) = signature {
                    p["thoughtSignature"] = json!(signature);
                }
                parts.push(p);
            }
            LlmStreamEvent::Usage { usage } => self.usage = usage.clone(),
            LlmStreamEvent::Finish { reason, .. } => {
                finish = Some(super::semantic::gemini_finish_reason(reason.as_deref()))
            }
            _ => {}
        }
        if !parts.is_empty() || finish.is_some() || matches!(event, LlmStreamEvent::Usage { .. }) {
            let mut value = json!({"responseId":self.id,"modelVersion":self.model,"candidates":[]});
            if !parts.is_empty() || finish.is_some() {
                value["candidates"] = json!([{"index":0,"content":{"role":"model","parts":parts},"finishReason":finish}]);
            }
            if matches!(event, LlmStreamEvent::Usage { .. }) {
                value["usageMetadata"] = json!({"promptTokenCount":self.usage.input_tokens,"candidatesTokenCount":self.usage.output_tokens,"totalTokenCount":self.usage.total_tokens});
            }
            output.push_str(&format_sse(None, &value)?);
        }
        Ok(())
    }
}

fn next_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4));
    let (position, delimiter) = match (lf, crlf) {
        (Some(left), Some(right)) => std::cmp::min(left, right),
        (Some(found), None) | (None, Some(found)) => found,
        (None, None) => return None,
    };
    let remainder = buffer.split_off(position + delimiter);
    buffer.truncate(position);
    let frame = std::mem::replace(buffer, remainder);
    Some(frame)
}

fn sse_frame_data(frame: &str) -> String {
    frame
        .lines()
        .filter_map(|line| line.trim().strip_prefix("data:").map(str::trim))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_sse(event: Option<&str>, value: &Value) -> Result<String> {
    let data = serde_json::to_string(value)?;
    Ok(match event {
        Some(event) => format!("event: {event}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    })
}

fn to_llm_usage(usage: &TokenUsage) -> LlmUsage {
    LlmUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    }
}

fn from_llm_usage(usage: &LlmUsage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_stream_roundtrips_through_typed_events() {
        let mut translator = TypedStreamTranslator::new(
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
            "client-model",
        )
        .unwrap();
        let output = translator.push_chunk(b"data: {\"id\":\"x\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n").unwrap();
        let tail = translator.finish_stream().unwrap();
        assert!(std::str::from_utf8(&output)
            .unwrap()
            .contains("content_block_delta"));
        assert!(std::str::from_utf8(&tail).unwrap().contains("message_stop"));
        assert_eq!(
            translator.semantic_response().response.candidates[0]
                .message
                .parts[0],
            LlmContentPart::text("hi")
        );
        assert_eq!(translator.metrics().usage.total_tokens, 2);
    }

    #[test]
    fn passthrough_preserves_unicode_split_at_every_byte_boundary() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"你好🌍\"}}]}\n\n".as_bytes();
        for split in 0..=input.len() {
            let mut translator = TypedStreamTranslator::new(
                ProtocolBridge::Passthrough,
                ProtocolKind::ChatCompletions,
                "client-model",
            )
            .unwrap();
            let mut output = Vec::new();
            output.extend_from_slice(&translator.push_chunk(&input[..split]).unwrap());
            output.extend_from_slice(&translator.push_chunk(&input[split..]).unwrap());
            output.extend_from_slice(&translator.finish_stream().unwrap());
            assert_eq!(output, input, "split at byte {split}");
            assert_eq!(translator.upstream_snapshot(), input);
        }
    }

    #[test]
    fn translated_stream_decodes_unicode_only_after_complete_frame() {
        let input = "data: {\"id\":\"x\",\"choices\":[{\"delta\":{\"content\":\"你好🌍\"}}]}\n\n";
        let split = input.find('好').unwrap() + 1;
        let mut translator = TypedStreamTranslator::new(
            ProtocolBridge::MessagesToCompletions,
            ProtocolKind::Messages,
            "client-model",
        )
        .unwrap();
        assert!(translator
            .push_chunk(&input.as_bytes()[..split])
            .unwrap()
            .is_empty());
        let output = translator.push_chunk(&input.as_bytes()[split..]).unwrap();
        assert!(std::str::from_utf8(&output).unwrap().contains("你好🌍"));
        assert_eq!(translator.upstream_snapshot(), input.as_bytes());
    }

    #[test]
    fn passthrough_preserves_malformed_and_non_utf8_provider_bytes() {
        let input = b"data: \xff\xfe\n\ndata: not-json\n\n";
        let mut translator = TypedStreamTranslator::new(
            ProtocolBridge::Passthrough,
            ProtocolKind::ChatCompletions,
            "client-model",
        )
        .unwrap();
        let output = translator.push_chunk(input).unwrap();
        assert_eq!(output.as_ref(), input);
        assert_eq!(translator.upstream_snapshot(), input);
    }

    #[test]
    fn responses_text_events_include_required_logprobs_fields() {
        let input = b"data: {\"id\":\"x\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
        let mut translator = TypedStreamTranslator::new(
            ProtocolBridge::ResponsesToCompletions,
            ProtocolKind::Responses,
            "client-model",
        )
        .unwrap();
        let output = translator.push_chunk(input).unwrap();
        let tail = translator.finish_stream().unwrap();
        let output = std::str::from_utf8(&output).unwrap();
        assert!(
            output.contains(r#""type":"response.output_text.delta""#),
            "{output}"
        );
        assert!(output.contains(r#""logprobs":[]"#));
        let tail = std::str::from_utf8(&tail).unwrap();
        assert!(tail.contains(r#""type":"response.output_text.done""#));
        assert!(tail.contains(r#""logprobs":[]"#));
        let completed = tail
            .split("\n\n")
            .find_map(|frame| {
                let data = frame.lines().find_map(|line| line.strip_prefix("data: "))?;
                let value: Value = serde_json::from_str(data).ok()?;
                (value["type"] == "response.completed").then_some(value)
            })
            .expect("response.completed event");
        assert_eq!(completed["response"]["output"][0]["type"], "message");
        assert_eq!(
            completed["response"]["output"][0]["content"][0]["text"],
            "hi"
        );
    }

    #[test]
    fn orphan_tool_delta_reserves_a_unique_responses_output_index() {
        let mut renderer = StreamRenderer::new(LlmProtocol::Responses, "client-model");
        let orphan = renderer
            .push_events(&[LlmStreamEvent::ToolArgumentsDelta {
                candidate: 0,
                id: "call-orphan".into(),
                delta: "{\"city\":".into(),
            }])
            .unwrap();
        assert!(orphan.contains("response.output_item.added"));
        assert!(orphan.contains("\"output_index\":0"));

        let text = renderer
            .push_events(&[LlmStreamEvent::TextDelta {
                candidate: 0,
                text: "hello".into(),
            }])
            .unwrap();
        assert!(text.contains("\"output_index\":1"));

        let late_start = renderer
            .push_events(&[LlmStreamEvent::ToolCallStart {
                candidate: 0,
                id: "call-orphan".into(),
                name: "weather".into(),
                signature: None,
            }])
            .unwrap();
        assert!(!late_start.contains("response.output_item.added"));

        let finished = renderer.finish(&[]).unwrap();
        assert!(finished.contains("\"output_index\":0"));
        assert!(finished.contains("\"name\":\"weather\""));
        let completed = finished
            .split("\n\n")
            .find_map(|frame| {
                let data = frame.lines().find_map(|line| line.strip_prefix("data: "))?;
                let value: Value = serde_json::from_str(data).ok()?;
                (value["type"] == "response.completed").then_some(value)
            })
            .expect("response.completed event");
        assert_eq!(completed["response"]["output"].as_array().unwrap().len(), 2);
        assert_eq!(completed["response"]["output"][0]["type"], "function_call");
        assert_eq!(completed["response"]["output"][1]["type"], "message");
    }
}
