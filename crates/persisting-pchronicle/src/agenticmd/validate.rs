//! Minimal safety validation for generated AgenticMD comments.
//!
//! Field presence and semantic combinations are deliberately not validated:
//! AgenticMD is a debugging view, not a protocol boundary.

use crate::formats::unknown_fields::{
    UnknownFieldLimits, compute_unknown_key_counts, normalize_agenticmd_unknown_pointer,
    validate_json_pointer,
};
use crate::{InputIssue, InputResult, StorylineDocument};

use super::codec::{MarkdownBlock, MarkdownHeader};

pub fn block_speaker(header: &MarkdownHeader) -> &str {
    header
        .fields
        .get("source")
        .and_then(|v| v.as_str())
        .or_else(|| header.fields.get("role").and_then(|v| v.as_str()))
        .unwrap_or("system")
}

pub fn validate_speaker(speaker: &str) -> InputResult<()> {
    let s = speaker.trim();
    if s.is_empty() {
        return Err(InputIssue::invalid("block speaker must not be empty"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(InputIssue::invalid(format!(
            "invalid block speaker: {speaker}"
        )));
    }
    Ok(())
}

pub fn validate_type_name(type_name: &str) -> InputResult<()> {
    let t = type_name.trim();
    if t.is_empty() {
        return Err(InputIssue::invalid("block type must not be empty"));
    }
    if t.contains('\n') || t.contains(':') {
        return Err(InputIssue::invalid(
            "block type must not contain ':' or newline",
        ));
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    {
        return Err(InputIssue::invalid(format!(
            "invalid block type: {type_name}"
        )));
    }
    Ok(())
}

pub fn validate_agenticmd_block(block: &MarkdownBlock) -> InputResult<()> {
    validate_type_name(&block.header.type_name)?;
    validate_speaker(block_speaker(&block.header))?;
    Ok(())
}

/// Native AgenticMD unknown fields use its logical `blocks` array for key counts,
/// unlike foreign sources whose pointers retain their own format semantics.
pub(crate) fn validate_agenticmd_storyline(story: &StorylineDocument) -> InputResult<()> {
    let expected_counts = story.unknown_fields.validate_with(
        UnknownFieldLimits::default(),
        normalize_agenticmd_unknown_pointer,
    )?;
    if expected_counts != story.unknown_key_counts {
        return Err(InputIssue::invalid(
            "storyline unknown_key_counts do not match AgenticMD unknown_fields",
        ));
    }

    // Reuse Storyline's canonical identity and turn validation without making
    // its format-neutral count normalizer authoritative for AgenticMD.
    let mut common = story.clone();
    common.unknown_key_counts = compute_unknown_key_counts(&common.unknown_fields)?;
    common.validate()
}

/// Only these leaf locations can be restored into an AgenticMD document.
/// The pointer token itself may be empty or escaped; it names a frontmatter
/// or header field and is deliberately not interpreted here.
pub(crate) fn validate_agenticmd_unknown_pointer(pointer: &str) -> InputResult<()> {
    validate_json_pointer(pointer)?;
    let tokens = pointer.split('/').collect::<Vec<_>>();
    let is_frontmatter_field = matches!(tokens.as_slice(), ["", "frontmatter", _]);
    let is_header_field =
        matches!(tokens.as_slice(), ["", "blocks", _, "header", _]) && is_array_index(tokens[2]);
    if is_frontmatter_field || is_header_field {
        Ok(())
    } else {
        Err(InputIssue::invalid(format!(
            "AgenticMD unknown-field pointer '{pointer}' must target /frontmatter/<key> or /blocks/<n>/header/<key>"
        )))
    }
}

fn is_array_index(token: &str) -> bool {
    !token.is_empty()
        && !(token.len() > 1 && token.starts_with('0'))
        && token.parse::<usize>().is_ok()
}

#[cfg(all(test, feature = "proptest"))]
mod proptests {
    use proptest::prelude::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    use super::*;

    fn valid_token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[A-Za-z0-9_-]{1,32}").unwrap()
    }

    proptest! {
        #[test]
        fn generated_speakers_and_type_names_are_accepted(
            speaker in valid_token_strategy(),
            type_name in proptest::string::string_regex("[A-Za-z0-9._/-]{1,32}").unwrap(),
        ) {
            prop_assert!(validate_speaker(&speaker).is_ok());
            prop_assert!(validate_type_name(&type_name).is_ok());

            let block = MarkdownBlock {
                header: MarkdownHeader {
                    type_name: type_name.clone(),
                    length: 0,
                    fields: BTreeMap::from([(String::from("source"), json!(speaker))]),
                },
                body: String::new(),
            };
            prop_assert!(validate_agenticmd_block(&block).is_ok());
        }

        #[test]
        fn invalid_speaker_characters_are_rejected(
            token in valid_token_strategy(),
            separator in prop::sample::select(vec![' ', ':', '/', '\n', '\t']),
        ) {
            let speaker = format!("{token}{separator}suffix");
            prop_assert!(validate_speaker(&speaker).is_err());
        }

        #[test]
        fn unknown_pointers_only_accept_frontmatter_or_indexed_headers(
            key in valid_token_strategy(),
            index in 0usize..16,
        ) {
            let frontmatter = format!("/frontmatter/{key}");
            let header = format!("/blocks/{}/header/{}", index, key);
            let body = format!("/blocks/{}/body/{}", index, key);
            let turns = format!("/turns/{}/{}", index, key);
            prop_assert!(validate_agenticmd_unknown_pointer(&frontmatter).is_ok());
            prop_assert!(validate_agenticmd_unknown_pointer(&header).is_ok());
            prop_assert!(validate_agenticmd_unknown_pointer(&body).is_err());
            prop_assert!(validate_agenticmd_unknown_pointer(&turns).is_err());
        }

        #[test]
        fn array_index_tokens_accept_only_canonical_decimal_forms(index in 0usize..100_000) {
            let canonical = format!("/blocks/{index}/header/source");
            prop_assert!(validate_agenticmd_unknown_pointer(&canonical).is_ok());
            let padded = format!("/blocks/0{index}/header/source");
            if index > 0 {
                prop_assert!(validate_agenticmd_unknown_pointer(&padded).is_err());
            }
        }
    }
}
