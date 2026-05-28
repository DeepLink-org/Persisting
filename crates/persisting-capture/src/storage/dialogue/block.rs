use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::fields::{attach_llm_fields, attach_subagent_link_fields, role_and_body};
use crate::record::{engine_line_to_record, CaptureRecord};
use crate::storage::markdown::{
    strip_subagent_footer_from_body, BlockHeader, MarkdownBlock, BLOCK_FORMAT_VERSION,
};
use crate::subagent_link::append_subagent_refs_footer;

pub fn capture_record_to_block(rec: &CaptureRecord) -> Result<(BlockHeader, Vec<u8>)> {
    let mut fields = BTreeMap::from([
        ("v".into(), json!(BLOCK_FORMAT_VERSION)),
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
    let content = strip_subagent_footer_from_body(&block.value_utf8()?.to_string());
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
