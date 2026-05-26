//! Extract human-readable dialogue text from LLM request/response payloads.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::Value;

use crate::conversion::{decode_stream_arguments_delta, unquote_chat_tool_arguments};

/// Last substantive user turn text (may include formatted `tool_result` blocks).
pub fn extract_user_message_from_request_body(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    extract_user_from_messages(&v).or_else(|| extract_user_from_responses_input(&v))
}

/// Count user-visible turns in a request body (Anthropic `messages` or Responses `input`).
pub fn count_visible_user_messages(v: &Value) -> usize {
    if let Some(messages) = v.get("messages").and_then(|m| m.as_array()) {
        return messages
            .iter()
            .filter(|msg| user_message_has_visible_text(msg))
            .count();
    }
    if let Some(input) = v.get("input") {
        return count_responses_input_user_turns(input);
    }
    0
}

fn user_message_has_visible_text(msg: &Value) -> bool {
    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
        return false;
    }
    if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
        let s = unwrap_session_tag(s);
        return !is_system_injection(s) && !s.trim().is_empty();
    }
    if let Some(parts) = msg.get("content").and_then(|c| c.as_array()) {
        for part in parts {
            match part.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                        let t = unwrap_session_tag(t);
                        if !is_system_injection(t) && !t.trim().is_empty() {
                            return true;
                        }
                    }
                }
                Some("tool_result") => {
                    if format_tool_result_block(part).is_some() {
                        return true;
                    }
                }
                _ => {}
            }
        }
    }
    false
}

fn count_responses_input_user_turns(input: &Value) -> usize {
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

fn extract_user_from_messages(v: &Value) -> Option<String> {
    let messages = v.get("messages")?.as_array()?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
            let s = unwrap_session_tag(s);
            if !is_system_injection(s) {
                return Some(s.to_string());
            }
            continue;
        }
        if let Some(parts) = msg.get("content").and_then(|c| c.as_array()) {
            let mut out = Vec::new();
            for part in parts {
                match part.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                            let t = unwrap_session_tag(t);
                            if !is_system_injection(t) {
                                out.push(t.to_string());
                            }
                        }
                    }
                    Some("tool_result") => {
                        if let Some(formatted) = format_tool_result_block(part) {
                            out.push(formatted);
                        }
                    }
                    _ => {}
                }
            }
            if !out.is_empty() {
                return Some(out.join("\n\n"));
            }
        }
    }
    None
}

/// Last user-visible text from OpenAI Responses `input` (Codex).
fn extract_user_from_responses_input(v: &Value) -> Option<String> {
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

/// Prefer trailing `function_call_output` items; otherwise last user message.
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

fn is_codex_context_injection(s: &str) -> bool {
    if is_system_injection(s) {
        return true;
    }
    let t = s.trim_start();
    t.starts_with("<permissions")
        || t.starts_with("<environment")
        || t.starts_with("You are Codex")
        || t.contains("</permissions instructions>")
}

/// Visible assistant turn from SSE: Anthropic, OpenAI completions, or Responses events.
pub fn extract_assistant_turn_from_sse(raw: &str) -> String {
    let anthropic = format_content_blocks(&parse_sse_content_blocks(raw));
    if !anthropic.is_empty() {
        return anthropic;
    }
    let completions = extract_openai_completions_sse_turn(raw);
    if !completions.is_empty() {
        return completions;
    }
    extract_responses_sse_turn(raw)
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
    if let Some(content) = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
    {
        if let Some(text) = content_to_string(content) {
            return Some(text);
        }
    }
    if let Some(message) = body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
    {
        if let Some(formatted) = format_openai_tool_calls(message.get("tool_calls")) {
            return Some(formatted);
        }
    }
    if let Some(parts) = body.get("content").and_then(|c| c.as_array()) {
        return Some(format_content_blocks_from_json_parts(parts));
    }
    if let Some(output) = body.get("output").and_then(|o| o.as_array()) {
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
        if !texts.is_empty() {
            return Some(texts.join("\n\n"));
        }
    }
    None
}

#[derive(Debug, Clone)]
pub enum ContentBlock {
    Text(String),
    ToolUse { name: String, input_json: String },
}

fn parse_sse_content_blocks(raw: &str) -> Vec<ContentBlock> {
    let mut parser = SseStreamBlockParser::default();
    parser.push_chunk(raw);
    parser.finish()
}

/// Incremental Anthropic SSE parser; emits completed blocks on `content_block_stop`.
#[derive(Default)]
pub struct SseStreamBlockParser {
    line_carry: String,
    by_index: BTreeMap<usize, BlockBuilder>,
    order: Vec<usize>,
    completed: Vec<ContentBlock>,
}

impl SseStreamBlockParser {
    pub fn push_chunk(&mut self, chunk: &str) -> Vec<ContentBlock> {
        self.line_carry.push_str(chunk);
        let mut newly = Vec::new();
        while let Some(nl) = self.line_carry.find('\n') {
            let line = self.line_carry[..nl].trim().to_string();
            self.line_carry.drain(..=nl);
            if let Some(block) = self.process_line(&line) {
                newly.push(block.clone());
                self.completed.push(block);
            }
        }
        newly
    }

    pub fn accumulated_assistant_text(&self) -> String {
        format_content_blocks(&self.completed)
    }

    pub fn finish(mut self) -> Vec<ContentBlock> {
        for idx in self.order.clone() {
            if self.by_index.contains_key(&idx) {
                if let Some(builder) = self.by_index.remove(&idx) {
                    if let Some(block) = builder.finish() {
                        self.completed.push(block);
                    }
                }
            }
        }
        self.completed
    }

    fn process_line(&mut self, line: &str) -> Option<ContentBlock> {
        let Some(json_str) = line.strip_prefix("data:") else {
            return None;
        };
        let json_str = json_str.trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            return None;
        }
        let Ok(v) = serde_json::from_str::<Value>(json_str) else {
            return None;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("content_block_start") => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let block_type = v
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("text");
                let tool_name = v
                    .get("content_block")
                    .and_then(|b| b.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string);
                if !self.by_index.contains_key(&index) {
                    self.order.push(index);
                }
                self.by_index.insert(
                    index,
                    BlockBuilder {
                        kind: match block_type {
                            "tool_use" => BlockKind::ToolUse {
                                name: tool_name.unwrap_or_else(|| "tool".into()),
                            },
                            "thinking" => BlockKind::Thinking,
                            _ => BlockKind::Text,
                        },
                        text: String::new(),
                        json: String::new(),
                    },
                );
                None
            }
            Some("content_block_delta") => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let Some(delta) = v.get("delta") else {
                    return None;
                };
                let entry = self.by_index.entry(index).or_insert_with(|| {
                    self.order.push(index);
                    BlockBuilder {
                        kind: BlockKind::Text,
                        text: String::new(),
                        json: String::new(),
                    }
                });
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                            entry.text.push_str(t);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(t) = delta.get("partial_json").and_then(|x| x.as_str()) {
                            entry.json.push_str(t);
                        }
                    }
                    _ => {}
                }
                None
            }
            Some("content_block_stop") => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                self.by_index.remove(&index).and_then(|b| b.finish())
            }
            _ => None,
        }
    }
}

pub fn push_sse_tool_snapshot(parser: &mut SseStreamBlockParser, chunk: &str) -> Option<String> {
    let new = parser.push_chunk(chunk);
    if new
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
    {
        Some(parser.accumulated_assistant_text())
    } else {
        None
    }
}

#[derive(Default)]
struct BlockBuilder {
    kind: BlockKind,
    text: String,
    json: String,
}

#[derive(Default)]
enum BlockKind {
    #[default]
    Text,
    ToolUse {
        name: String,
    },
    Thinking,
}

impl BlockBuilder {
    fn finish(self) -> Option<ContentBlock> {
        match self.kind {
            BlockKind::Thinking => None,
            BlockKind::ToolUse { name } => {
                let input = normalize_json(&self.json);
                if input.is_empty() && self.text.is_empty() {
                    return None;
                }
                Some(ContentBlock::ToolUse {
                    name,
                    input_json: if input.is_empty() { self.text } else { input },
                })
            }
            BlockKind::Text => {
                if self.text.is_empty() {
                    None
                } else {
                    Some(ContentBlock::Text(self.text))
                }
            }
        }
    }
}

fn format_content_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .map(format_content_block)
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_content_blocks_from_json_parts(parts: &[Value]) -> String {
    let mut blocks = Vec::new();
    for part in parts {
        match part.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = part.get("text").and_then(|x| x.as_str()) {
                    blocks.push(ContentBlock::Text(t.to_string()));
                }
            }
            Some("tool_use") => {
                let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let input = part
                    .get("input")
                    .map(|v| serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()))
                    .unwrap_or_else(|| "{}".into());
                blocks.push(ContentBlock::ToolUse {
                    name: name.to_string(),
                    input_json: input,
                });
            }
            _ => {}
        }
    }
    format_content_blocks(&blocks)
}

fn format_content_block(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(t) => t.clone(),
        ContentBlock::ToolUse { name, input_json } => {
            format!("```tool:{name}\n{input_json}\n```")
        }
    }
}

fn format_tool_result_block(part: &Value) -> Option<String> {
    let id = part
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("tool");
    let content = tool_result_content(part.get("content")?);
    Some(format!("```tool_result:{id}\n{content}\n```"))
}

fn tool_result_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| p.get("content").and_then(|c| c.as_str()))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn normalize_json(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return serde_json::to_string_pretty(&v).unwrap_or_else(|_| trimmed.to_string());
    }
    trimmed.to_string()
}

fn unwrap_session_tag(s: &str) -> &str {
    let t = s.trim();
    if let Some(inner) = t
        .strip_prefix("<session>")
        .and_then(|x| x.strip_suffix("</session>"))
    {
        return inner.trim();
    }
    t
}

fn is_system_injection(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("<system-reminder>")
        || t.starts_with("<system-reminder")
        || t.starts_with("<local-command")
        || t.starts_with("[SUGGESTION MODE:")
        || t.starts_with("SUGGESTION MODE:")
}

fn extract_openai_completions_sse_turn(raw: &str) -> String {
    let mut parser = CompletionsSseToolParser::default();
    parser.ingest_chunk(raw);
    parser.finish();
    parser.snapshot()
}

fn join_assistant_parts(text: &str, tools: impl IntoIterator<Item = (String, String)>) -> String {
    let mut parts = Vec::new();
    if !text.trim().is_empty() {
        parts.push(text.to_string());
    }
    for (name, args) in tools {
        if name.trim().is_empty() && args.trim().is_empty() {
            continue;
        }
        let name = if name.trim().is_empty() {
            "tool".to_string()
        } else {
            name
        };
        let args = {
            let normalized = normalize_json(&args);
            if normalized.trim().is_empty() {
                "{}".to_string()
            } else {
                normalized
            }
        };
        parts.push(format!("```tool:{name}\n{args}\n```"));
    }
    parts.join("\n\n")
}

fn format_openai_tool_calls(tool_calls: Option<&Value>) -> Option<String> {
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

fn extract_responses_sse_turn(raw: &str) -> String {
    use std::collections::BTreeMap;

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

fn content_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(unwrap_session_tag(s).to_string()),
        Value::Array(parts) => {
            let formatted = format_content_blocks_from_json_parts(parts);
            if formatted.is_empty() {
                None
            } else {
                Some(formatted)
            }
        }
        _ => None,
    }
}

/// Main agent turn delivered with a subagent's `x-claude-code-agent-id` header
/// (e.g. `<task-notification>` after background agent completion).
pub fn is_main_agent_continuation_payload(payload: &Value) -> bool {
    if payload
        .get("user_content")
        .and_then(|s| s.as_str())
        .is_some_and(contains_task_notification)
    {
        return true;
    }
    payload
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|msgs| msgs.iter().any(message_contains_task_notification))
}

pub fn is_main_agent_continuation(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .is_some_and(|v| is_main_agent_continuation_payload(&v))
}

fn message_contains_task_notification(msg: &Value) -> bool {
    match msg.get("content") {
        Some(Value::String(s)) => contains_task_notification(s),
        Some(Value::Array(parts)) => parts.iter().any(|p| {
            p.get("text")
                .and_then(|t| t.as_str())
                .is_some_and(contains_task_notification)
        }),
        _ => false,
    }
}

fn contains_task_notification(s: &str) -> bool {
    s.contains("<task-notification>")
}

/// Claude Code subagent probe (`*-flash`/`*-haiku` + `<session>` wrapper).
pub fn is_subagent_shape_payload(payload: &Value) -> bool {
    let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("");
    if !model.contains("flash") && !model.contains("haiku") {
        return false;
    }
    if payload
        .get("user_content")
        .and_then(|s| s.as_str())
        .is_some_and(|s| s.contains("<session>"))
    {
        return true;
    }
    payload
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|msgs| {
            msgs.iter().any(|msg| {
                msg.get("content")
                    .map(content_contains_session_tag)
                    .unwrap_or(false)
            })
        })
}

fn content_contains_session_tag(c: &Value) -> bool {
    match c {
        Value::String(s) => s.contains("<session>"),
        Value::Array(parts) => parts.iter().any(|p| {
            p.get("text")
                .and_then(|t| t.as_str())
                .is_some_and(|t| t.contains("<session>"))
        }),
        _ => false,
    }
}

/// Alias for proxy summary payloads (`user_content` field).
pub fn is_subagent_request_payload(payload: &Value) -> bool {
    is_subagent_shape_payload(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn count_visible_user_messages_increments_with_new_user_turns() {
        let one = json!({"messages":[{"role":"user","content":"hi"}]});
        assert_eq!(count_visible_user_messages(&one), 1);
        let two = json!({"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"user","content":"hi again"}
        ]});
        assert_eq!(count_visible_user_messages(&two), 2);
        let replay = json!({"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"user","content":"hi again"}
        ]});
        assert_eq!(count_visible_user_messages(&replay), 2);
    }

    #[test]
    fn anthropic_request_skips_system_reminder() {
        let body = br#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"<system-reminder>\nskills\n</system-reminder>"},
            {"type":"text","text":"hi"}
        ]}]}"#;
        assert_eq!(
            extract_user_message_from_request_body(&Bytes::from_static(body)).as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn unwraps_session_tag() {
        let body = r#"{"messages":[{"role":"user","content":[{"type":"text","text":"<session>\nhi\n</session>"}]}]}"#;
        assert_eq!(
            extract_user_message_from_request_body(&Bytes::copy_from_slice(body.as_bytes()))
                .as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn skips_suggestion_mode_prompt() {
        let body = br#"{"messages":[{"role":"user","content":[{"type":"text","text":"[SUGGESTION MODE: predict next input]\nReply ONLY the suggestion."}]}]}"#;
        assert!(extract_user_message_from_request_body(&Bytes::from_static(body)).is_none());
    }

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
    fn sse_incremental_parser_emits_tool_on_stop() {
        let sse = r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Launching."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","name":"Agent","id":"toolu_1"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"prompt\":\"scan\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}
"#;
        let mut parser = SseStreamBlockParser::default();
        let snap1 = push_sse_tool_snapshot(&mut parser, sse);
        assert!(snap1.is_some());
        assert!(snap1.unwrap().contains("```tool:Agent"));
    }

    #[test]
    fn anthropic_sse_extracts_tool_use() {
        let sse = r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我来 review。"}}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\": \"git diff\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}
"#;
        let out = extract_assistant_turn_from_sse(sse);
        assert!(out.contains("我来 review。"));
        assert!(out.contains("```tool:Bash"));
        assert!(out.contains("git diff"));
    }

    #[test]
    fn request_includes_tool_result() {
        let body = br#"{"messages":[{"role":"user","content":[
            {"type":"tool_result","tool_use_id":"toolu_1","content":"file1.rs\nfile2.rs"}
        ]}]}"#;
        let out = extract_user_message_from_request_body(&Bytes::from_static(body)).unwrap();
        assert!(out.contains("```tool_result:toolu_1"));
        assert!(out.contains("file1.rs"));
    }

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
        let out = extract_user_message_from_request_body(&Bytes::from(body)).unwrap();
        assert!(out.contains("```tool_result:call_1"));
        assert!(out.contains("Cargo.toml"));
    }

    #[test]
    fn completions_sse_tool_parser_emits_tool_snapshot() {
        let raw = include_str!("../../tests/fixtures/response/completions/stream_tool_call.txt");
        let mut parser = CompletionsSseToolParser::default();
        let snap = parser.push_chunk(raw).expect("tool snapshot");
        assert!(snap.contains("```tool:shell"));
        assert!(snap.contains("ls"));
    }

    #[test]
    fn completions_sse_extracts_tool_calls_without_trailing_newline() {
        let raw = include_str!("../../tests/fixtures/response/completions/stream_tool_call.txt");
        let trimmed = raw.trim_end();
        let out = extract_assistant_turn_from_sse(trimmed);
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
        let out = extract_assistant_turn_from_sse(sse);
        assert!(out.contains("```tool:read_file"));
        assert!(out.contains("a.rs"));
        assert!(out.contains("b.rs"));
    }

    #[test]
    fn thinking_only_sse_yields_empty_visible_text() {
        let sse = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"stay silent"}}

event: message_stop
data: {"type":"message_stop"}
"#;
        assert!(extract_assistant_turn_from_sse(sse).is_empty());
    }

    #[test]
    fn detects_task_notification_as_main_continuation() {
        let body = br#"{"model":"deepseek-v4-pro","messages":[{"role":"user","content":[{"type":"text","text":"<task-notification>\n<task-id>abc12345</task-id>\n</task-notification>"}]}]}"#;
        assert!(is_main_agent_continuation(&Bytes::from_static(body)));
    }

    #[test]
    fn subagent_flash_prompt_is_not_main_continuation() {
        let body = br#"{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"Review docs"}]}"#;
        assert!(!is_main_agent_continuation(&Bytes::from_static(body)));
    }

    #[test]
    fn codex_responses_input_extracts_last_user_text() {
        let body = Bytes::from_static(include_bytes!(
            "../../tests/fixtures/requests/responses/codex_basic.json"
        ));
        assert_eq!(
            extract_user_message_from_request_body(&body).as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn openai_completions_sse_extracts_assistant_text() {
        let raw = include_str!("../../tests/fixtures/response/completions/stream_head.txt");
        let out = extract_assistant_turn_from_sse(raw);
        assert!(out.contains("Hi"));
        assert!(out.contains("help"));
    }

    #[test]
    fn openai_completions_sse_extracts_tool_calls() {
        let raw = include_str!("../../tests/fixtures/response/completions/stream_tool_call.txt");
        let out = extract_assistant_turn_from_sse(raw);
        assert!(out.contains("```tool:shell"));
        assert!(out.contains("ls"));
    }

    #[test]
    fn responses_sse_extracts_output_text_delta() {
        let sse = r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Hello"}

event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":" world"}
"#;
        assert_eq!(extract_assistant_turn_from_sse(sse), "Hello world");
    }
}
