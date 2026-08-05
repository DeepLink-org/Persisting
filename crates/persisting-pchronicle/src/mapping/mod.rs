//! EventRecord ⇄ AgenticMD debug-view mapping.
//!
//! New blocks use Storyline-like fields. Reverse conversion exists for explicit
//! imports, but AgenticMD is intentionally not a lossless persistence boundary.

mod fields;
mod text;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::formats::agenticmd::{AgenticmdBlock, AgenticmdHeader};
use crate::formats::agenticmd_body::{
    append_subagent_refs_footer, strip_subagent_footer_from_body, BLOCK_FORMAT_VERSION,
};
use crate::formats::events::{EventIdentity, EventRecord};

use fields::{attach_llm_fields, attach_subagent_link_fields, role_and_body};

/// Build an agenticmd block from an event record (primary write mapping).
///
/// Uses JSON-oriented visible-text extraction. For live SSE fidelity, stamp
/// `user_content` / `assistant_content` on the payload before calling, or use
/// [`event_record_to_agenticmd_block_with_text`].
pub fn event_record_to_agenticmd_block(rec: &EventRecord) -> Result<AgenticmdBlock> {
    let (role, body) = role_and_body(rec)?;
    event_record_to_agenticmd_block_with_text(rec, &role, &body)
}

/// Like [`event_record_to_agenticmd_block`] but uses caller-supplied role/body
/// (e.g. capture's SSE-aware `visible_*` extractors).
pub fn event_record_to_agenticmd_block_with_text(
    rec: &EventRecord,
    role: &str,
    body: &str,
) -> Result<AgenticmdBlock> {
    let source = match role {
        "user" => "user",
        "assistant" | "agent" => "agent",
        _ => "system",
    };
    let mut fields = std::collections::BTreeMap::from([
        ("v".into(), json!(BLOCK_FORMAT_VERSION)),
        ("kind".into(), json!(rec.kind)),
        ("source".into(), json!(source)),
        ("producer".into(), json!(rec.source)),
        ("event_seq".into(), json!(rec.seq)),
        ("step_id".into(), json!(rec.seq / 2 + 1)),
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
    attach_llm_fields(&mut fields, rec);

    let body = append_subagent_refs_footer(body, &rec.payload);
    Ok(AgenticmdBlock {
        header: AgenticmdHeader {
            type_name: "markdown".into(),
            length: body.len(),
            fields,
        },
        body,
    })
}

pub fn agenticmd_block_to_replay_json(block: &AgenticmdBlock) -> Result<String> {
    let mut o = serde_json::Map::new();
    o.insert("type".into(), json!(&block.header.type_name));
    o.insert("length".into(), json!(block.header.length));
    for (k, v) in &block.header.fields {
        o.insert(k.clone(), v.clone());
    }
    o.insert("content".into(), json!(&block.body));
    serde_json::to_string(&Value::Object(o)).context("replay JSON")
}

/// Reconstruct an [`EventRecord`] from an agenticmd block (primary read mapping).
pub fn agenticmd_block_to_event_record(block: &AgenticmdBlock) -> Result<EventRecord> {
    let content = strip_subagent_footer_from_body(&block.body);
    let kind = block.kind().unwrap_or("markdown").to_string();
    let role = block.role().unwrap_or("note");
    let seq = ["event_seq", "seq"]
        .iter()
        .find_map(|key| block.header.fields.get(*key).and_then(|v| v.as_u64()))
        .or_else(|| block.step_id().and_then(|id| u64::try_from(id).ok()))
        .unwrap_or(0);

    let payload = match kind.as_str() {
        "llm.request" | "http.request" => {
            let mut p = json!({ "body": { "messages": [{"role": role, "content": content}] } });
            if let Some(model) = block.header.fields.get("model").and_then(|v| v.as_str()) {
                p["model"] = json!(model);
            }
            if let Some(path) = block.header.fields.get("path").and_then(|v| v.as_str()) {
                p["path"] = json!(path);
            }
            p
        }
        "llm.response" | "llm.response.stream" | "http.response" | "http.response.stream" => {
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

    Ok(EventRecord {
        identity: EventIdentity::default(),
        seq,
        source: block
            .header
            .fields
            .get("producer")
            .and_then(|v| v.as_str())
            .unwrap_or("agenticmd-view")
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

/// Attach source view metadata for explicit imports.
pub fn enrich_event_from_agenticmd_block(
    mut rec: EventRecord,
    block: &AgenticmdBlock,
) -> EventRecord {
    rec.payload["_agenticmd"] = json!({
        "source": block.source(),
        "block_fields": block.header.fields,
    });
    rec
}

/// Map AgenticMD blocks to event records for explicit import.
pub fn agenticmd_blocks_to_event_records(blocks: &[AgenticmdBlock]) -> Result<Vec<EventRecord>> {
    blocks
        .iter()
        .enumerate()
        .map(|(i, block)| {
            let rec =
                agenticmd_block_to_event_record(block).with_context(|| format!("block[{i}]"))?;
            Ok(enrich_event_from_agenticmd_block(rec, block))
        })
        .collect()
}

/// Parse agenticmd markdown (lenient) into enriched event records.
pub fn markdown_document_to_event_records(doc: &str) -> Result<Vec<EventRecord>> {
    let parsed = crate::formats::agenticmd::parse_agenticmd_document(doc)?;
    agenticmd_blocks_to_event_records(&parsed.blocks)
}
