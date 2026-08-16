//! agenticmd ⇄ storyline.
//!
//! AgenticMD is a human/debugging view. Conversion prefers Storyline field
//! names while retaining legacy aliases for older capture documents.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::convert::message_text;
use crate::formats::agenticmd::{
    AgenticmdBlock, AgenticmdDocument, AgenticmdHeader, AGENTICMD_FORMAT_NAME,
    AGENTICMD_FRONTMATTER_FORMAT,
};
use crate::formats::storyline::{StorylineAgent, StorylineDocument, StorylineTurn};
use crate::Result;

/// Header field names preserved via `turn.extra` for hub round-trips.
const EXTRA_CORRELATION_KEYS: &[&str] = &[
    "call_id",
    "event_seq",
    "step_id",
    "producer",
    "seq",
    "turn",
    "trace_id",
    "parent_uuid",
    "draft",
    "source",
];

pub fn agenticmd_to_storyline(doc: &AgenticmdDocument) -> Result<StorylineDocument> {
    let session_id = doc.session_id.clone().unwrap_or_else(|| "unknown".into());
    let agent_id = doc.agent_id.clone().unwrap_or_else(|| "unknown".into());

    let mut turns = Vec::new();
    for (i, block) in doc.blocks.iter().enumerate() {
        let id = block.step_id().unwrap_or((i as i64) + 1);
        let source = block.source().unwrap_or("system");
        let model = block
            .header
            .fields
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let latency_ms = block
            .header
            .fields
            .get("latency_ms")
            .and_then(|v| v.as_i64());
        let ttft_ms = block.header.fields.get("ttft_ms").and_then(|v| v.as_i64());
        let kind = block
            .header
            .fields
            .get("kind")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let timestamp = block
            .header
            .fields
            .get("timestamp")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        turns.push(StorylineTurn {
            id,
            kind,
            timestamp,
            source: source.into(),
            message: Value::String(block.body.clone()),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: model,
            llm_call_count: if source == "agent" { Some(1) } else { None },
            is_copied_context: None,
            latency_ms,
            ttft_ms,
            extra: Some(agenticmd_block_extra(block)),
        });
    }

    Ok(StorylineDocument {
        run_id: None,
        session_id,
        agent: StorylineAgent {
            id: agent_id.clone(),
            name: Some(agent_id),
            version: None,
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: None,
        turns,
    })
}

pub fn storyline_to_agenticmd(story: &StorylineDocument) -> Result<AgenticmdDocument> {
    story.validate()?;
    let mut blocks = Vec::new();
    for turn in &story.turns {
        let body = message_text(&turn.message).unwrap_or_default();
        let mut fields = BTreeMap::new();
        fields.insert("source".into(), json!(turn.source));
        fields.insert("step_id".into(), json!(turn.id));
        let kind = turn
            .kind
            .clone()
            .unwrap_or_else(|| turn.effective_kind().to_string());
        fields.insert("kind".into(), json!(kind));
        if let Some(model) = &turn.model_name {
            fields.insert("model".into(), json!(model));
        }
        if let Some(ms) = turn.latency_ms {
            fields.insert("latency_ms".into(), json!(ms));
        }
        if let Some(ms) = turn.ttft_ms {
            fields.insert("ttft_ms".into(), json!(ms));
        }
        if let Some(ts) = &turn.timestamp {
            fields.insert("timestamp".into(), json!(ts));
        }
        restore_agenticmd_extra_fields(&mut fields, turn.extra.as_ref());

        let type_name = turn
            .extra
            .as_ref()
            .and_then(|e| e.get("block_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();

        blocks.push(AgenticmdBlock {
            header: AgenticmdHeader {
                type_name,
                length: body.len(),
                fields,
            },
            body,
        });
    }

    Ok(AgenticmdDocument {
        format: AGENTICMD_FORMAT_NAME.into(),
        frontmatter_format: AGENTICMD_FRONTMATTER_FORMAT.into(),
        session_id: Some(story.session_id.clone()),
        agent_id: Some(story.agent.id.clone()),
        frontmatter: BTreeMap::new(),
        blocks,
    })
}

fn agenticmd_block_extra(block: &AgenticmdBlock) -> Value {
    let mut extra = Map::new();
    extra.insert("block_type".into(), json!(&block.header.type_name));
    for key in EXTRA_CORRELATION_KEYS {
        if let Some(v) = block.header.fields.get(*key) {
            extra.insert((*key).into(), v.clone());
        }
    }
    // Prefer header session/agent when present (document-level may be unset).
    for key in ["session_id", "agent_id"] {
        if let Some(v) = block.header.fields.get(key) {
            extra.insert(key.into(), v.clone());
        }
    }
    Value::Object(extra)
}

fn restore_agenticmd_extra_fields(fields: &mut BTreeMap<String, Value>, extra: Option<&Value>) {
    let Some(extra) = extra.and_then(|v| v.as_object()) else {
        return;
    };
    for key in EXTRA_CORRELATION_KEYS
        .iter()
        .chain(["session_id", "agent_id"].iter())
    {
        if let Some(v) = extra.get(*key) {
            fields.insert((*key).into(), v.clone());
        }
    }
}
