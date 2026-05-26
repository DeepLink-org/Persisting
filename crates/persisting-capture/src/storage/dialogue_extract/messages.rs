//! Anthropic Messages API: user turns, content blocks, SSE parsing.

use std::collections::BTreeMap;

use serde_json::Value;

use super::common::{
    format_tool_result_block, is_system_injection, normalize_json, unwrap_session_tag,
};

pub(crate) fn extract_user_from_messages(v: &Value) -> Option<String> {
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

pub(crate) fn user_message_has_visible_text(msg: &Value) -> bool {
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

pub(crate) fn anthropic_sse_turn(raw: &str) -> String {
    format_content_blocks(&parse_sse_content_blocks(raw))
}

pub(crate) fn anthropic_assistant_from_json(body: &Value) -> Option<String> {
    if let Some(parts) = body.get("content").and_then(|c| c.as_array()) {
        let formatted = format_content_blocks_from_json_parts(parts);
        if !formatted.is_empty() {
            return Some(formatted);
        }
    }
    None
}

pub(crate) fn content_to_string(v: &Value) -> Option<String> {
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
        assert_eq!(anthropic_sse_turn(sse), "Hi!");
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
        let out = anthropic_sse_turn(sse);
        assert!(out.contains("我来 review。"));
        assert!(out.contains("```tool:Bash"));
        assert!(out.contains("git diff"));
    }

    #[test]
    fn thinking_only_sse_yields_empty_visible_text() {
        let sse = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"stay silent"}}

event: message_stop
data: {"type":"message_stop"}
"#;
        assert!(anthropic_sse_turn(sse).is_empty());
    }
}
