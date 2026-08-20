//! agenticmd ⇄ storyline.
//!
//! AgenticMD is a human/debugging view backed by authoritative Storyline
//! metadata and readable message blocks.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::formats::storyline::{StorylineDocument, StorylineTurn};
use crate::formats::unknown_fields::{
    canonical_source_document_id, normalize_agenticmd_unknown_pointer, restore_json_pointer,
    PointerWrite, UnknownFieldLimits,
};
use crate::{InputIssue, InputResult, Result};

use super::codec::{
    encode_agenticmd_block, encode_agenticmd_preamble, parse_agenticmd_document, MarkdownBlock,
    MarkdownDocument, MarkdownHeader, AGENTICMD_FRONTMATTER_FORMAT,
};
use super::validate::{validate_agenticmd_storyline, validate_agenticmd_unknown_pointer};

const STORYLINE_METADATA_KEY: &str = "storyline";
const MESSAGE_ENCODING_KEY: &str = "message_encoding";

/// Parse AgenticMD into its authoritative Storyline model.
pub fn parse_agenticmd(input: &str) -> InputResult<StorylineDocument> {
    let document = parse_agenticmd_document(input)?;
    let metadata = document
        .frontmatter
        .get(STORYLINE_METADATA_KEY)
        .ok_or_else(|| InputIssue::invalid("missing authoritative Storyline metadata"))?;
    let mut story = metadata
        .as_object()
        .cloned()
        .ok_or_else(|| InputIssue::invalid("expected an object").at("frontmatter.storyline"))?;
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
                .ok_or_else(|| {
                    InputIssue::invalid("expected an object")
                        .at(format!("block[{index}].storyline"))
                })?;
            let message = match block
                .header
                .fields
                .get(MESSAGE_ENCODING_KEY)
                .and_then(Value::as_str)
            {
                Some("json") => serde_json::from_str(&block.body).map_err(|error| {
                    InputIssue::invalid(error.to_string()).at(format!("block[{index}].body"))
                })?,
                _ => Value::String(block.body.clone()),
            };
            turn.insert("msg".into(), message);
            serde_json::from_value::<StorylineTurn>(Value::Object(turn)).map_err(|error| {
                InputIssue::invalid(error.to_string()).at(format!("block[{index}].storyline"))
            })
        })
        .collect::<InputResult<Vec<_>>>()?;
    story.insert(
        "turns".into(),
        serde_json::to_value(&turns).map_err(|error| InputIssue::invalid(error.to_string()))?,
    );
    let mut story = serde_json::from_value::<StorylineDocument>(Value::Object(story))
        .map_err(|error| InputIssue::invalid(error.to_string()).at("frontmatter.storyline"))?;
    validate_agenticmd_storyline(&story)?;
    capture_agenticmd_unknown_fields(&document, &mut story)?;
    Ok(story)
}

/// Encode a Storyline as its human-readable AgenticMD representation.
pub fn encode_agenticmd(story: &StorylineDocument) -> Result<String> {
    validate_agenticmd_storyline(story)?;
    let frontmatter = storyline_frontmatter(story)?;
    let blocks = story
        .turns
        .iter()
        .map(|turn| storyline_turn_block(turn, None))
        .collect::<Result<Vec<_>>>()?;
    let mut logical_document = json!({
        "frontmatter": frontmatter,
        "blocks": blocks,
    });
    restore_agenticmd_unknown_fields(story, &mut logical_document)?;

    let frontmatter =
        serde_json::from_value::<BTreeMap<String, Value>>(logical_document["frontmatter"].clone())?;
    let blocks = serde_json::from_value::<Vec<MarkdownBlock>>(logical_document["blocks"].clone())?;

    let mut output = encode_agenticmd_preamble(&frontmatter)?;
    for block in blocks {
        output.push_str(&encode_agenticmd_block(&block)?);
    }
    Ok(output)
}

pub(super) fn encode_storyline_preamble(story: &StorylineDocument) -> Result<String> {
    validate_agenticmd_storyline(story)?;
    encode_agenticmd_preamble(&storyline_frontmatter(story)?)
}

fn storyline_frontmatter(story: &StorylineDocument) -> Result<BTreeMap<String, Value>> {
    // Native fields are written back to their Markdown locations below. The
    // existing Storyline metadata remains the carrier for all foreign sources.
    let mut metadata_story = story.clone();
    metadata_story.unknown_fields.sources.remove("agenticmd");
    metadata_story.unknown_key_counts.remove("agenticmd");

    let mut metadata = serde_json::to_value(metadata_story)?
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("serialized Storyline must be an object"))?;
    metadata.remove("turns");
    Ok(BTreeMap::from([
        (
            "format".into(),
            Value::String(AGENTICMD_FRONTMATTER_FORMAT.into()),
        ),
        (STORYLINE_METADATA_KEY.into(), Value::Object(metadata)),
    ]))
}

pub(super) fn storyline_turn_block(
    turn: &StorylineTurn,
    edit_key: Option<&str>,
) -> Result<MarkdownBlock> {
    let mut turn_metadata = serde_json::to_value(turn)?
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("serialized Storyline turn must be an object"))?;
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

fn capture_agenticmd_unknown_fields(
    document: &MarkdownDocument,
    story: &mut StorylineDocument,
) -> InputResult<()> {
    let source_document_id = agenticmd_source_document_id(document)?;
    for (key, value) in &document.frontmatter {
        if is_consumed_frontmatter_field(key) {
            continue;
        }
        story.unknown_fields.insert(
            "agenticmd",
            &source_document_id,
            format!("/frontmatter/{}", encode_pointer_token(key)),
            value.clone(),
        )?;
    }

    for (index, block) in document.blocks.iter().enumerate() {
        for (key, value) in &block.header.fields {
            if is_consumed_header_field(key) {
                continue;
            }
            story.unknown_fields.insert(
                "agenticmd",
                &source_document_id,
                format!("/blocks/{index}/header/{}", encode_pointer_token(key)),
                value.clone(),
            )?;
        }
    }

    let recomputed_counts = story.unknown_fields.validate_with(
        UnknownFieldLimits::default(),
        normalize_agenticmd_unknown_pointer,
    )?;
    match recomputed_counts.get("agenticmd") {
        Some(counts) => {
            story
                .unknown_key_counts
                .insert("agenticmd".into(), counts.clone());
        }
        None => {
            story.unknown_key_counts.remove("agenticmd");
        }
    }
    Ok(())
}

fn agenticmd_source_document_id(document: &MarkdownDocument) -> InputResult<String> {
    let mut source =
        serde_json::to_value(document).map_err(|error| InputIssue::invalid(error.to_string()))?;
    let frontmatter = source
        .get_mut("frontmatter")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| {
            InputIssue::invalid("serialized AgenticMD document lacks object frontmatter")
        })?;
    frontmatter.remove(STORYLINE_METADATA_KEY);
    canonical_source_document_id(&source).map_err(|error| InputIssue::invalid(error.to_string()))
}

fn restore_agenticmd_unknown_fields(
    story: &StorylineDocument,
    logical_document: &mut Value,
) -> Result<()> {
    let Some(source) = story.unknown_fields.sources.get("agenticmd") else {
        return Ok(());
    };
    for (pointer, value) in &source.fields {
        validate_agenticmd_unknown_pointer(pointer)?;
        restore_json_pointer(
            logical_document,
            pointer,
            value.clone(),
            PointerWrite::InsertOnly,
        )?;
    }
    Ok(())
}

fn is_consumed_frontmatter_field(key: &str) -> bool {
    matches!(key, "format" | STORYLINE_METADATA_KEY)
}

fn is_consumed_header_field(key: &str) -> bool {
    matches!(
        key,
        "source" | "step_id" | MESSAGE_ENCODING_KEY | STORYLINE_METADATA_KEY
    )
}

fn encode_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::{encode_agenticmd, parse_agenticmd};
    use crate::formats::unknown_fields::normalize_agenticmd_unknown_pointer;
    use crate::{StoryLink, StorylineDocument, StorylineToolCall, StorylineTurn};
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
                result: None,
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

    #[test]
    fn agenticmd_frontmatter_carries_unknown_sources() {
        let mut story = StorylineDocument::new("s", "a");
        story
            .unknown_fields
            .insert("atif", "source", "/vendor", json!(7))
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();

        let encoded = encode_agenticmd(&story).unwrap();
        let decoded = parse_agenticmd(&encoded).unwrap();

        assert_eq!(decoded.unknown_fields, story.unknown_fields);
        assert_eq!(decoded.unknown_key_counts, story.unknown_key_counts);
    }

    #[test]
    fn agenticmd_captures_and_restores_native_unknown_fields_at_logical_pointers() {
        let input = r#"---
format: persisting
storyline:
  session: s
  agent:
    id: a
vendor_top: 7
---

<!-- persisting:block:user {"type":"text","length":2,"source":"user","step_id":1,"message_encoding":"text","storyline":{"id":1,"src":"user"},"vendor_header":null} -->

hi
"#;

        let parsed = parse_agenticmd(input).unwrap();
        let fields = &parsed.unknown_fields.sources["agenticmd"].fields;
        assert_eq!(fields["/frontmatter/vendor_top"], json!(7));
        assert_eq!(
            fields["/blocks/0/header/vendor_header"],
            serde_json::Value::Null
        );
        assert_eq!(
            parsed.unknown_key_counts["agenticmd"]["/blocks/*/header/vendor_header"],
            1
        );

        let encoded = encode_agenticmd(&parsed).unwrap();
        let restored = parse_agenticmd(&encoded).unwrap();
        assert_eq!(restored, parsed);
    }

    #[test]
    fn agenticmd_rejects_native_unknown_field_collisions() {
        let mut story = StorylineDocument::new("s", "a");
        story
            .unknown_fields
            .insert(
                "agenticmd",
                "source",
                "/frontmatter/format",
                json!("vendor"),
            )
            .unwrap();
        story.refresh_unknown_key_counts().unwrap();

        assert!(encode_agenticmd(&story).is_err());
    }

    #[test]
    fn agenticmd_rejects_native_block_header_collisions() {
        let mut story = StorylineDocument::new("s", "a");
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
            timestamp: None,
            source: "user".into(),
            message: json!("hello"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
        });
        story
            .unknown_fields
            .insert(
                "agenticmd",
                "source",
                "/blocks/0/header/source",
                json!("vendor"),
            )
            .unwrap();
        story.unknown_key_counts = story
            .unknown_fields
            .validate_with(
                crate::formats::unknown_fields::UnknownFieldLimits::default(),
                normalize_agenticmd_unknown_pointer,
            )
            .unwrap();

        assert!(encode_agenticmd(&story).is_err());
    }

    #[test]
    fn agenticmd_rejects_mismatched_serialized_unknown_key_counts() {
        let input = r#"---
format: persisting
storyline:
  session: s
  agent:
    id: a
  unknown_fields:
    sources:
      atif:
        source_document_id: source
        fields:
          /vendor: 7
  unknown_key_counts:
    atif:
      /vendor: 2
---
"#;

        assert!(parse_agenticmd(input).is_err());
    }
}
