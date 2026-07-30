//! agenticmd ⇄ storyline.

use std::collections::BTreeMap;

use serde_json::json;

use crate::convert::message_text;
use crate::formats::agenticmd::{
    AgenticmdBlock, AgenticmdDocument, AgenticmdHeader, AGENTICMD_FORMAT_NAME,
    AGENTICMD_FRONTMATTER_FORMAT,
};
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineTurn, STORYLINE_SCHEMA_VERSION,
};
use crate::Result;

pub fn agenticmd_to_storyline(doc: &AgenticmdDocument) -> Result<StorylineDocument> {
    let session_id = doc.session_id.clone().unwrap_or_else(|| "unknown".into());
    let agent_id = doc.agent_id.clone().unwrap_or_else(|| "unknown".into());

    let mut turns = Vec::new();
    for (i, block) in doc.blocks.iter().enumerate() {
        let id = (i as i64) + 1;
        let role = block.role().unwrap_or("note");
        let source = match role {
            "user" => "user",
            "assistant" => "agent",
            _ => "system",
        };
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

        turns.push(StorylineTurn {
            id,
            kind: None,
            timestamp: None,
            source: source.into(),
            message: serde_json::Value::String(block.body.clone()),
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
            extra: None,
        });
    }

    Ok(StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
        run_id: None,
        session_id,
        agent: StorylineAgent {
            id: agent_id.clone(),
            name: Some(agent_id),
            version: Some("0".into()),
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
        let role = match turn.source.as_str() {
            "user" => "user",
            "system" => "note",
            _ => "assistant",
        };
        let body = message_text(&turn.message).unwrap_or_default();
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!(turn.effective_kind()));
        if let Some(model) = &turn.model_name {
            fields.insert("model".into(), json!(model));
        }
        if let Some(ms) = turn.latency_ms {
            fields.insert("latency_ms".into(), json!(ms));
        }
        if let Some(ms) = turn.ttft_ms {
            fields.insert("ttft_ms".into(), json!(ms));
        }
        blocks.push(AgenticmdBlock {
            header: AgenticmdHeader {
                type_name: "text".into(),
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
