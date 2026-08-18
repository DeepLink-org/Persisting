//! agenticmd ⇄ storyline.
//!
//! AgenticMD is a human/debugging view. Conversion prefers Storyline field
//! names while retaining legacy aliases for older capture documents.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::formats::storyline::{StorylineAgent, StorylineDocument, StorylineTurn};
use crate::{DocumentFormat, Error, Result};

use super::codec::{
    encode_agenticmd_block, encode_agenticmd_preamble, parse_agenticmd_document, MarkdownBlock,
    MarkdownDocument, MarkdownHeader, AGENTICMD_FRONTMATTER_FORMAT,
};

const STORYLINE_METADATA_KEY: &str = "storyline";
const MESSAGE_ENCODING_KEY: &str = "message_encoding";

/// Parse AgenticMD into its authoritative Storyline model.
pub fn parse_agenticmd(input: &str) -> Result<StorylineDocument> {
    let document = parse_agenticmd_document(input)?;
    let Some(metadata) = document.frontmatter.get(STORYLINE_METADATA_KEY) else {
        return agenticmd_to_storyline(&document);
    };
    let mut story = metadata
        .as_object()
        .cloned()
        .ok_or_else(|| Error::InvalidDocument {
            format: DocumentFormat::AgenticMd,
            path: None,
            location: Some("frontmatter.storyline".into()),
            message: "expected an object".into(),
        })?;
    let turns = document
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let mut turn = block
                .header
                .fields
                .get(STORYLINE_METADATA_KEY)
                .and_then(Value::as_object)
                .cloned()
                .ok_or_else(|| Error::InvalidDocument {
                    format: DocumentFormat::AgenticMd,
                    path: None,
                    location: Some(format!("block[{index}].storyline")),
                    message: "expected an object".into(),
                })?;
            let message = match block
                .header
                .fields
                .get(MESSAGE_ENCODING_KEY)
                .and_then(Value::as_str)
            {
                Some("json") => {
                    serde_json::from_str(&block.body).map_err(|error| Error::InvalidDocument {
                        format: DocumentFormat::AgenticMd,
                        path: None,
                        location: Some(format!("block[{index}].body")),
                        message: error.to_string(),
                    })?
                }
                _ => Value::String(block.body.clone()),
            };
            turn.insert("msg".into(), message);
            serde_json::from_value::<StorylineTurn>(Value::Object(turn)).map_err(|error| {
                Error::InvalidDocument {
                    format: DocumentFormat::AgenticMd,
                    path: None,
                    location: Some(format!("block[{index}].storyline")),
                    message: error.to_string(),
                }
            })
        })
        .collect::<Result<Vec<_>>>()?;
    story.insert("turns".into(), serde_json::to_value(&turns)?);
    let document =
        serde_json::from_value::<StorylineDocument>(Value::Object(story)).map_err(|error| {
            Error::InvalidDocument {
                format: DocumentFormat::AgenticMd,
                path: None,
                location: Some("frontmatter.storyline".into()),
                message: error.to_string(),
            }
        })?;
    document.validate()?;
    Ok(document)
}

/// Encode a Storyline as its human-readable AgenticMD representation.
pub fn encode_agenticmd(story: &StorylineDocument) -> Result<String> {
    story.validate()?;
    let mut output = encode_storyline_preamble(story)?;
    for turn in &story.turns {
        output.push_str(&encode_agenticmd_block(&storyline_turn_block(turn, None)?)?);
    }
    Ok(output)
}

pub(super) fn encode_storyline_preamble(story: &StorylineDocument) -> Result<String> {
    let mut metadata = serde_json::to_value(story)?
        .as_object()
        .cloned()
        .ok_or_else(|| Error::Other("serialized Storyline must be an object".into()))?;
    metadata.remove("turns");
    let frontmatter: BTreeMap<String, Value> = BTreeMap::from([
        (
            "format".into(),
            Value::String(AGENTICMD_FRONTMATTER_FORMAT.into()),
        ),
        (STORYLINE_METADATA_KEY.into(), Value::Object(metadata)),
    ]);
    encode_agenticmd_preamble(&frontmatter)
}

pub(super) fn storyline_turn_block(
    turn: &StorylineTurn,
    edit_key: Option<&str>,
) -> Result<MarkdownBlock> {
    let mut turn_metadata = serde_json::to_value(turn)?
        .as_object()
        .cloned()
        .ok_or_else(|| Error::Other("serialized Storyline turn must be an object".into()))?;
    turn_metadata.remove("msg");
    let (body, encoding, type_name) = match &turn.message {
        Value::String(text) => (text.clone(), "text", "text"),
        value => (serde_json::to_string_pretty(value)?, "json", "json"),
    };
    let mut fields = BTreeMap::from([
        ("source".into(), Value::String(turn.source.clone())),
        ("step_id".into(), json!(turn.id)),
        (MESSAGE_ENCODING_KEY.into(), Value::String(encoding.into())),
        (STORYLINE_METADATA_KEY.into(), Value::Object(turn_metadata)),
    ]);
    if let Some(edit_key) = edit_key {
        fields.insert("call_id".into(), Value::String(edit_key.into()));
    }
    Ok(MarkdownBlock {
        header: MarkdownHeader {
            type_name: type_name.into(),
            length: body.len(),
            fields,
        },
        body,
    })
}

fn agenticmd_to_storyline(doc: &MarkdownDocument) -> Result<StorylineDocument> {
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
            extra: None,
        });
    }

    Ok(StorylineDocument {
        schema_version: None,
        run_id: None,
        trajectory_id: None,
        attempt_id: None,
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
        presence: Default::default(),
        turns,
    })
}

#[cfg(test)]
mod tests {
    use super::{encode_agenticmd, parse_agenticmd};
    use crate::{FieldPresence, StoryLink, StorylineDocument, StorylineToolCall, StorylineTurn};
    use serde_json::json;

    #[test]
    fn agenticmd_storyline_roundtrip_preserves_the_authoritative_model() {
        let mut story = StorylineDocument::new("session-1", "agent-1");
        story.schema_version = Some("ATIF-v1.7".into());
        story.run_id = Some("run-1".into());
        story.attempt_id = Some("attempt-1".into());
        story.agent.version = Some("1.2".into());
        story.agent.model_name = Some("model-1".into());
        story.agent.tool_definitions = Some(json!([{"name":"lookup"}]));
        story.agent.extra = Some(json!({"team":"infra"}));
        story.parent = Some(StoryLink {
            parent_session_id: "parent-1".into(),
            spawn_call_id: Some("spawn-1".into()),
            spawn_id: Some(9),
            relation: "spawn".into(),
        });
        story.child_session_ids = Some(vec!["child-1".into()]);
        story.notes = Some("readable trajectory".into());
        story.final_metrics = Some(json!({"score": 1}));
        story.continued_trajectory_ref = Some("next-1".into());
        story.extra = Some(json!({"unknown":null}));
        story.turns.push(StorylineTurn {
            id: 7,
            kind: Some("autonomous".into()),
            timestamp: Some("2026-08-17T01:02:03Z".into()),
            source: "agent".into(),
            message: json!([{"type":"text","text":"hello"}]),
            reasoning_content: Some("reason".into()),
            reasoning_effort: Some(json!("high")),
            tool_calls: Some(vec![StorylineToolCall {
                tool_call_id: "call-1".into(),
                function_name: "lookup".into(),
                arguments: json!({"q":"x"}),
                result: FieldPresence::Null,
                duration_ms: Some(12),
                extra: Some(json!({"provider":"test"})),
            }]),
            observation: Some(json!({
                "results":[{"source_call_id":"call-1","content":"ok"}]
            })),
            metrics: Some(json!({"tokens":3})),
            model_name: Some("model-1".into()),
            llm_call_count: Some(1),
            is_copied_context: Some(false),
            latency_ms: Some(50),
            ttft_ms: Some(5),
            extra: Some(json!({"trace_id":"trace-1"})),
        });

        let markdown = encode_agenticmd(&story).unwrap();
        let restored = parse_agenticmd(&markdown).unwrap();
        assert_eq!(restored, story);
    }
}
