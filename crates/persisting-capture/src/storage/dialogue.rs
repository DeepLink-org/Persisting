//! `CaptureRecord` ↔ markdown trajectory blocks.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::dialogue_extract::{extract_assistant_text_from_json, extract_assistant_turn_from_sse};
use super::markdown::{BlockHeader, MarkdownBlock};
use super::markdown_pipeline::MarkdownPipeline;
use super::record::{engine_line_to_record, record_to_engine_line, CaptureRecord};
use super::subagent_link::append_subagent_refs_footer;

pub use super::markdown_pipeline::skip_markdown_block;

/// Returns `None` when the record should not appear in session markdown (stateless batch: no replay dedup).
pub fn try_capture_record_to_block(rec: &CaptureRecord) -> Result<Option<(BlockHeader, Vec<u8>)>> {
    MarkdownPipeline::default().try_block(rec)
}

/// Batch convert records to markdown blocks (replay dedup applied in seq order).
pub fn capture_records_to_blocks(records: &[CaptureRecord]) -> Result<Vec<(BlockHeader, Vec<u8>)>> {
    MarkdownPipeline::blocks_from_records(records)
}

pub fn capture_record_to_block(rec: &CaptureRecord) -> Result<(BlockHeader, Vec<u8>)> {
    let mut fields = BTreeMap::from([
        ("kind".into(), json!(rec.kind)),
        ("source".into(), json!(rec.source)),
        ("seq".into(), json!(rec.seq)),
        ("turn".into(), json!(rec.seq / 2 + 1)),
    ]);
    if rec.payload.get("draft").and_then(|v| v.as_bool()) == Some(true) {
        fields.insert("draft".into(), json!(true));
    }
    if let Some(sid) = &rec.session_id {
        fields.insert("session_id".into(), json!(sid));
    }
    if let Some(aid) = &rec.agent_id {
        fields.insert("agent_id".into(), json!(aid));
    }
    if let Some(ts) = &rec.timestamp {
        fields.insert("timestamp".into(), json!(ts));
    }
    if let Some(p) = &rec.parent_uuid {
        fields.insert("parent_uuid".into(), json!(p));
    }
    if let Some(t) = &rec.trace_id {
        fields.insert("trace_id".into(), json!(t));
    }
    if let Some(c) = &rec.call_id {
        fields.insert("call_id".into(), json!(c));
    }
    attach_subagent_link_fields(&mut fields, rec);

    let (role, body) = role_and_body(rec)?;
    fields.insert("role".into(), json!(role));
    attach_llm_fields(&mut fields, rec);

    let body_bytes = append_subagent_refs_footer(&body, rec).into_bytes();

    Ok((
        BlockHeader {
            type_name: "markdown".into(),
            length: 0,
            fields,
        },
        body_bytes,
    ))
}

pub fn engine_line_to_block(line: &str) -> Result<(BlockHeader, Vec<u8>)> {
    capture_record_to_block(&engine_line_to_record(line)?)
}

pub fn block_to_replay_json(block: &MarkdownBlock) -> Result<String> {
    let mut o = serde_json::Map::new();
    o.insert("type".into(), json!(block.type_name()));
    o.insert("length".into(), json!(block.header.length));
    for (k, v) in &block.header.fields {
        o.insert(k.clone(), v.clone());
    }
    o.insert("content".into(), json!(block.value_utf8()?));
    serde_json::to_string(&Value::Object(o)).context("replay JSON")
}

pub fn block_to_capture_record(block: &MarkdownBlock) -> Result<CaptureRecord> {
    let content = block.value_utf8()?.to_string();
    let kind = block.kind().unwrap_or("markdown").to_string();
    let role = block.role().unwrap_or("note");
    let seq = block
        .header
        .fields
        .get("seq")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let payload = match kind.as_str() {
        "llm.request" => {
            let mut p = json!({ "body": { "messages": [{"role": role, "content": content}] } });
            if let Some(model) = block.header.fields.get("model").and_then(|v| v.as_str()) {
                p["model"] = json!(model);
            }
            if let Some(path) = block.header.fields.get("path").and_then(|v| v.as_str()) {
                p["path"] = json!(path);
            }
            p
        }
        "llm.response" | "llm.response.stream" => {
            let status = block
                .header
                .fields
                .get("status")
                .and_then(|v| v.as_u64())
                .unwrap_or(200);
            let mut usage = serde_json::Map::new();
            for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
                if let Some(v) = block.header.fields.get(key) {
                    usage.insert(key.into(), v.clone());
                }
            }
            let mut body = serde_json::Map::new();
            body.insert(
                "choices".into(),
                json!([{"message": {"role": "assistant", "content": content}}]),
            );
            if !usage.is_empty() {
                body.insert("usage".into(), Value::Object(usage));
            }
            json!({ "status": status, "body": Value::Object(body) })
        }
        _ => json!({ "role": role, "content": content }),
    };

    Ok(CaptureRecord {
        seq,
        source: block
            .header
            .fields
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown")
            .into(),
        kind,
        timestamp: block
            .header
            .fields
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        session_id: block
            .header
            .fields
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        agent_id: block
            .header
            .fields
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        parent_uuid: block
            .header
            .fields
            .get("parent_uuid")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        trace_id: block
            .header
            .fields
            .get("trace_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        call_id: block
            .header
            .fields
            .get("call_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        subagent_id: block
            .header
            .fields
            .get("subagent_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        parent_agent_id: block
            .header
            .fields
            .get("parent_agent_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        branch: None,
        parent_call_id: None,
        payload,
    })
}

pub fn import_markdown_to_engine_lines(doc: &str) -> Result<String> {
    super::markdown::parse_document(doc)?
        .iter()
        .enumerate()
        .map(|(i, b)| {
            record_to_engine_line(&block_to_capture_record(b)?)
                .with_context(|| format!("block[{i}]"))
        })
        .collect::<Result<Vec<_>>>()
        .map(|v| v.join("\n"))
}

fn role_and_body(rec: &CaptureRecord) -> Result<(String, String)> {
    Ok(match rec.kind.as_str() {
        "llm.request" => (
            "user".into(),
            user_message(&rec.payload)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_default(),
        ),
        "llm.response" | "llm.response.stream" => (
            "assistant".into(),
            assistant_message(&rec.payload)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_default(),
        ),
        "user" | "assistant" | "system" | "tool" | "note" => (
            rec.kind.clone(),
            rec.payload
                .get("content")
                .and_then(content_to_string)
                .unwrap_or_else(|| compact_json(&rec.payload)),
        ),
        _ => ("note".into(), compact_json(&rec.payload)),
    })
}

fn attach_subagent_link_fields(fields: &mut BTreeMap<String, Value>, rec: &CaptureRecord) {
    if let Some(id) = &rec.subagent_id {
        fields.insert("subagent_id".into(), json!(id));
    }
    if let Some(id) = &rec.parent_agent_id {
        fields.insert("parent_agent_id".into(), json!(id));
    }
    for key in [
        "refs_subagent_ids",
        "subagent_trajectories",
        "subagent_trajectory",
        "spawn_hints",
        "spawn_links",
        "parent_agent_id",
    ] {
        if let Some(v) = rec.payload.get(key) {
            fields.insert(key.into(), v.clone());
        }
    }
}

fn attach_llm_fields(fields: &mut BTreeMap<String, Value>, rec: &CaptureRecord) {
    match rec.kind.as_str() {
        "llm.request" => {
            if let Some(model) = rec.payload.get("model").and_then(|v| v.as_str()) {
                fields.insert("model".into(), json!(model));
            }
            if let Some(path) = rec.payload.get("path").and_then(|v| v.as_str()) {
                fields.insert("path".into(), json!(path));
            }
        }
        "llm.response" | "llm.response.stream" => {
            if let Some(status) = rec.payload.get("status") {
                fields.insert("status".into(), status.clone());
            }
            if let Some(usage) = rec
                .payload
                .get("body")
                .and_then(|b| b.get("usage"))
                .or_else(|| rec.payload.get("usage"))
            {
                for key in [
                    "prompt_tokens",
                    "completion_tokens",
                    "total_tokens",
                    "input_tokens",
                    "output_tokens",
                ] {
                    if let Some(v) = usage.get(key) {
                        fields.insert(key.into(), v.clone());
                    }
                }
                if !fields.contains_key("prompt_tokens") {
                    if let Some(v) = usage.get("input_tokens") {
                        fields.insert("prompt_tokens".into(), v.clone());
                    }
                }
                if !fields.contains_key("completion_tokens") {
                    if let Some(v) = usage.get("output_tokens") {
                        fields.insert("completion_tokens".into(), v.clone());
                    }
                }
            }
            if let Some(v) = rec.payload.get("ttft_ms") {
                fields.insert("ttft_ms".into(), v.clone());
            }
        }
        _ => {}
    }
}

/// Queryable columns extracted from a capture record (shared by Lance + markdown block headers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRecordView {
    pub role: Option<String>,
    pub text: Option<String>,
    pub model: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Visible dialogue text and LLM summary fields; omits text when markdown would skip the block.
pub fn capture_record_view(rec: &CaptureRecord) -> CaptureRecordView {
    let mut view = CaptureRecordView::default();
    attach_llm_view(&mut view, rec);
    if skip_markdown_block(rec) {
        return view;
    }
    if let Ok((role, body)) = role_and_body(rec) {
        view.role = Some(role);
        if !body.is_empty() {
            view.text = Some(body);
        }
    }
    view
}

fn attach_llm_view(view: &mut CaptureRecordView, rec: &CaptureRecord) {
    match rec.kind.as_str() {
        "llm.request" => {
            if let Some(model) = rec.payload.get("model").and_then(|v| v.as_str()) {
                view.model = Some(model.to_string());
            }
            if let Some(path) = rec.payload.get("path").and_then(|v| v.as_str()) {
                view.path = Some(path.to_string());
            }
        }
        "llm.response" | "llm.response.stream" => {
            if let Some(status) = rec.payload.get("status").and_then(|v| v.as_i64()) {
                view.status = Some(status as i32);
            }
            if let Some(usage) = rec
                .payload
                .get("body")
                .and_then(|b| b.get("usage"))
                .or_else(|| rec.payload.get("usage"))
            {
                view.prompt_tokens = token_field(usage, "prompt_tokens", "input_tokens");
                view.completion_tokens = token_field(usage, "completion_tokens", "output_tokens");
                view.total_tokens = usage.get("total_tokens").and_then(|v| v.as_i64());
            }
        }
        _ => {}
    }
}

fn token_field(usage: &Value, primary: &str, alias: &str) -> Option<i64> {
    usage
        .get(primary)
        .or_else(|| usage.get(alias))
        .and_then(|v| v.as_i64())
}

/// Inner LLM JSON: `payload.body` or proxy wrapper `payload.body.body`.
fn llm_inner_body<'a>(payload: &'a Value) -> Option<&'a Value> {
    let wrap = payload.get("body")?;
    if wrap.get("messages").is_some() || wrap.get("choices").is_some() {
        Some(wrap)
    } else {
        wrap.get("body")
    }
}

fn user_message(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("user_content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    let messages = llm_inner_body(payload)
        .and_then(|b| b.get("messages"))
        .or_else(|| payload.get("messages"))?
        .as_array()?;
    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("user") {
            return msg.get("content").and_then(content_to_string);
        }
    }
    None
}

fn assistant_message(payload: &Value) -> Option<String> {
    if let Some(s) = payload.get("assistant_content").and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    if let Some(s) = payload.get("body").and_then(|b| b.as_str()) {
        let text = extract_assistant_turn_from_sse(s);
        if !text.is_empty() {
            return Some(text);
        }
    }
    if let Some(inner) = llm_inner_body(payload) {
        if let Some(s) = inner.as_str() {
            let text = extract_assistant_turn_from_sse(s);
            if !text.is_empty() {
                return Some(text);
            }
        }
        if let Some(text) = extract_assistant_text_from_json(inner) {
            return Some(text);
        }
    }
    llm_inner_body(payload)
        .and_then(|b| b.get("choices"))
        .or_else(|| payload.get("body").and_then(|b| b.get("choices")))
        .or_else(|| payload.get("choices"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(content_to_string)
        .or_else(|| payload.get("content").and_then(content_to_string))
}

fn content_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let out: Vec<_> = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect();
            if out.is_empty() {
                None
            } else {
                Some(out.join("\n"))
            }
        }
        _ => None,
    }
}

fn compact_json(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
}

/// Build a streaming draft assistant block (markdown view only; not written to Lance).
pub fn draft_stream_assistant_block(
    rec: &CaptureRecord,
    assistant_content: &str,
) -> Result<Option<(BlockHeader, Vec<u8>)>> {
    if assistant_content.trim().is_empty() {
        return Ok(None);
    }
    let mut draft = rec.clone();
    draft.kind = "llm.response.stream".into();
    draft.payload = json!({
        "status": rec.payload.get("status").and_then(|v| v.as_u64()).unwrap_or(200),
        "assistant_content": assistant_content,
        "draft": true,
    });
    if skip_markdown_block(&draft) {
        return Ok(None);
    }
    Ok(Some(capture_record_to_block(&draft)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CaptureLevel;
    use crate::markdown_trajectory::{encode_block_with_header, parse_document};
    use crate::sink::{
        llm_request_record, llm_request_summary_record, llm_response_record,
        llm_response_record_with_content,
    };
    use crate::Call;
    fn test_call() -> Call {
        Call {
            call_id: "call-test".into(),
            trace_id: "trace-test".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    const LEVEL: CaptureLevel = CaptureLevel::Dialogue;

    #[test]
    fn proxy_nested_body_writes_plain_content() {
        let req = llm_request_record(
            Some("sess".into()),
            Some("agent".into()),
            "mock-model",
            "/v1/chat/completions",
            &json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "body": {"messages":[{"role":"user","content":"你好"}],"model":"mock-model"},
            }),
        );
        let resp = llm_response_record(
            Some("sess".into()),
            Some("agent".into()),
            200,
            &json!({
                "protocol": "chat_completions",
                "provider": "openai",
                "usage": {"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3},
                "body": {
                    "choices":[{"message":{"role":"assistant","content":"你好！"}}],
                },
            }),
            false,
            &test_call(),
        );
        let (_, b1) = capture_record_to_block(&req).unwrap();
        let (_, b2) = capture_record_to_block(&resp).unwrap();
        assert_eq!(std::str::from_utf8(&b1).unwrap(), "你好");
        assert_eq!(std::str::from_utf8(&b2).unwrap(), "你好！");
    }

    #[test]
    fn llm_pair_writes_and_replays() {
        let req = llm_request_record(
            Some("sess".into()),
            None,
            "deepseek-chat",
            "/v1/chat/completions",
            &json!({"messages":[{"role":"user","content":"你好"}]}),
        );
        let resp = llm_response_record(
            Some("sess".into()),
            None,
            200,
            &json!({
                "choices":[{"message":{"role":"assistant","content":"你好！"}}],
                "usage":{"prompt_tokens":12,"completion_tokens":18,"total_tokens":30}
            }),
            false,
            &test_call(),
        );
        let (h1, b1) = capture_record_to_block(&req).unwrap();
        let (h2, b2) = capture_record_to_block(&resp).unwrap();
        let doc = format!(
            "{}{}",
            encode_block_with_header(h1, &b1).unwrap(),
            encode_block_with_header(h2, &b2).unwrap(),
        );
        let blocks = parse_document(&doc).unwrap();
        let row: Value = serde_json::from_str(&block_to_replay_json(&blocks[0]).unwrap()).unwrap();
        assert_eq!(row["role"], "user");
        assert_eq!(row["content"], "你好");
    }

    #[test]
    fn summary_request_writes_user_content_not_json() {
        let req = llm_request_summary_record(
            Some("sess".into()),
            Some("agent".into()),
            "deepseek-v4-pro",
            "/v1/messages",
            111_027,
            "messages",
            "anthropic",
            Some("hi".into()),
            None,
            &test_call(),
            LEVEL,
            None,
        );
        let (_, body) = capture_record_to_block(&req).unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "hi");
    }

    #[test]
    fn stream_response_with_assistant_content_writes_plain_text() {
        let resp = llm_response_record_with_content(
            Some("sess".into()),
            Some("agent".into()),
            200,
            &json!({"body": "event: x\ndata: {}\n"}),
            true,
            Some("Hi! How can I help you?".into()),
            &test_call(),
            LEVEL,
        );
        let (_, body) = capture_record_to_block(&resp).unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            "Hi! How can I help you?"
        );
    }

    #[test]
    fn skip_internal_suggestion_request_and_silent_response() {
        let req = llm_request_summary_record(
            Some("s".into()),
            None,
            "m",
            "/v1/messages",
            100,
            "messages",
            "anthropic",
            None,
            None,
            &test_call(),
            LEVEL,
            None,
        );
        assert!(skip_markdown_block(&req));
        let resp = llm_response_record_with_content(
            Some("s".into()),
            None,
            200,
            &json!({"body": "event: x\ndata: {}\n"}),
            true,
            Some(String::new()),
            &test_call(),
            LEVEL,
        );
        assert!(skip_markdown_block(&resp));
    }

    #[test]
    fn skip_count_tokens_request() {
        let req = llm_request_summary_record(
            Some("s".into()),
            None,
            "m",
            "/v1/messages/count_tokens",
            1000,
            "count_tokens",
            "anthropic",
            Some("huge context".into()),
            None,
            &test_call(),
            LEVEL,
            None,
        );
        assert!(skip_markdown_block(&req));
    }

    #[test]
    fn subagent_link_fields_in_markdown_block() {
        use crate::session_storage::CaptureRoute;
        use crate::subagent_link::enrich_record;

        let mut registry = crate::subagent_link::SubagentRegistry::default();
        let route = CaptureRoute {
            root_session: Some("run-1".into()),
            session_id: "sess".into(),
            storage_session_id: "agent-abc".into(),
            subagent_id: Some("abc".into()),
        };
        let mut rec = llm_response_record_with_content(
            Some("sess".into()),
            Some("proxy".into()),
            200,
            &json!({}),
            false,
            Some("done".into()),
            &test_call(),
            LEVEL,
        );
        let _outcome = enrich_record(
            &mut rec,
            &route,
            &axum::http::HeaderMap::new(),
            None,
            Some("done"),
            &mut registry,
        );
        let (header, body) = capture_record_to_block(&rec).unwrap();
        assert_eq!(
            header.fields.get("subagent_id").and_then(|v| v.as_str()),
            Some("abc")
        );
        assert_eq!(
            header
                .fields
                .get("subagent_trajectory")
                .and_then(|v| v.as_str()),
            Some("agent-abc.md")
        );
        assert!(std::str::from_utf8(&body)
            .unwrap()
            .contains("persisting:subagent-self"));
    }

    #[test]
    fn skip_main_flash_companion_user_duplicate() {
        let mut rec = llm_request_summary_record(
            Some("sess".into()),
            Some("proxy".into()),
            "deepseek-v4-flash",
            "/v1/messages",
            100,
            "messages",
            "anthropic",
            Some("再次开三个subagent".into()),
            None,
            &test_call(),
            LEVEL,
            None,
        );
        assert!(skip_markdown_block(&rec));
        rec.subagent_id = Some("abc".into());
        assert!(!skip_markdown_block(&rec));
    }
}
