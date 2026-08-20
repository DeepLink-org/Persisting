//! `agenticmd` — best-effort Markdown view for humans and debugging.
//!
//! It is intentionally not a canonical storage format. Storyline metadata
//! carries semantics; block headers provide a readable, editable view.
//! ```text
//! ---
//! format: persisting   # logical name in pChronicle: agenticmd
//! session: ...
//! ---
//!
//! <!-- persisting:block:user {"type":"text","length":N,...} -->
//!
//! body
//!
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{InputIssue, InputResult, Result};

pub const AGENTICMD_FORMAT_NAME: &str = "agenticmd";
pub const AGENTICMD_FRONTMATTER_FORMAT: &str = "persisting";
pub const BLOCK_MARKER: &str = "<!-- persisting:block";
/// Layout hint embedded in capture live-document YAML (`block:` field).
pub const AGENTICMD_BLOCK_LAYOUT: &str =
    "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkdownHeader {
    #[serde(rename = "type", default = "default_block_type")]
    pub type_name: String,
    #[serde(default)]
    pub length: usize,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkdownBlock {
    pub header: MarkdownHeader,
    pub body: String,
}

impl MarkdownBlock {
    /// Human-facing presentation role derived from Storyline `source`.
    pub fn role(&self) -> Option<&str> {
        if let Some(role) = self.header.fields.get("role").and_then(|v| v.as_str()) {
            return Some(role);
        }
        match self.source()? {
            "agent" => Some("assistant"),
            "system" => Some("note"),
            source => Some(source),
        }
    }

    /// Storyline-compatible source (`user`, `agent`, or `system`).
    pub fn source(&self) -> Option<&str> {
        if let Some(source @ ("user" | "agent" | "system")) =
            self.header.fields.get("source").and_then(|v| v.as_str())
        {
            return Some(source);
        }
        match self.header.fields.get("role").and_then(|v| v.as_str())? {
            "user" => Some("user"),
            "assistant" | "agent" => Some("agent"),
            _ => Some("system"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarkdownDocument {
    /// Logical pChronicle format name (`agenticmd`).
    pub format: String,
    /// Frontmatter `format:` value (usually `persisting`).
    pub frontmatter_format: String,
    #[serde(default)]
    pub frontmatter: BTreeMap<String, Value>,
    pub blocks: Vec<MarkdownBlock>,
}

impl MarkdownDocument {
    pub fn new(blocks: Vec<MarkdownBlock>) -> Self {
        Self {
            format: AGENTICMD_FORMAT_NAME.into(),
            frontmatter_format: AGENTICMD_FRONTMATTER_FORMAT.into(),
            frontmatter: BTreeMap::new(),
            blocks,
        }
    }
}

pub fn parse_agenticmd_document(input: &str) -> InputResult<MarkdownDocument> {
    let (frontmatter, _body, _off) = split_frontmatter_with_offset(input)?;
    let spans = parse_agenticmd_blocks_with_spans(input)?;
    let mut doc = MarkdownDocument::new(spans.into_iter().map(|s| s.block).collect());
    doc.frontmatter = frontmatter;
    if let Some(fmt) = doc.frontmatter.get("format").and_then(Value::as_str) {
        doc.frontmatter_format = fmt.to_string();
    }
    Ok(doc)
}

/// One parsed block plus its absolute byte range in the source document.
///
/// `start..end` covers the comment line through trailing blank lines — the same
/// span capture uses for markdown upsert rewrites.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownBlockSpan {
    pub block: MarkdownBlock,
    pub start: usize,
    pub end: usize,
}

/// Parse blocks with absolute byte spans (for capture upsert / diagnostics).
pub fn parse_agenticmd_blocks_with_spans(input: &str) -> InputResult<Vec<MarkdownBlockSpan>> {
    let (_frontmatter, body, body_offset) = split_frontmatter_with_offset(input)?;
    parse_blocks_with_spans(body, body_offset)
}

/// Byte offset where the document body begins (immediately after YAML frontmatter).
///
/// Returns `0` when there is no opening `---` frontmatter. Errors on unclosed
/// frontmatter. Does **not** skip `#` comment lines — callers that rewrite
/// session rollup frontmatter should preserve everything after this offset.
pub fn agenticmd_body_byte_offset(input: &str) -> InputResult<usize> {
    let (_fm, _body, offset) = split_frontmatter_with_offset(input)?;
    Ok(offset)
}

/// Encode a YAML frontmatter fence (`---\n…\n---\n\n`) from any serializable mapping.
///
/// Storyline metadata encoding is layered on top by `agenticmd::convert`.
pub fn encode_agenticmd_preamble<T: Serialize>(frontmatter: &T) -> Result<String> {
    let yaml = serde_yaml::to_string(frontmatter)?;
    Ok(format!("---\n{yaml}---\n\n"))
}

/// Encode a single agenticmd / capture TLV block (comment header + body).
///
/// Normative on-disk layout shared with `persisting-gateway` live write paths.
pub fn encode_agenticmd_block(block: &MarkdownBlock) -> Result<String> {
    let header = MarkdownHeader {
        type_name: block.header.type_name.clone(),
        length: block.body.len(),
        fields: block.header.fields.clone(),
    };
    let speaker = block.source().unwrap_or("system");
    let json = serde_json::to_string(&header)?;
    Ok(format!(
        "{BLOCK_MARKER}:{speaker} {json} -->\n\n{}\n\n",
        block.body
    ))
}

fn split_frontmatter_with_offset(
    input: &str,
) -> InputResult<(BTreeMap<String, Value>, &str, usize)> {
    if !input.starts_with("---") {
        return Ok((BTreeMap::new(), input, 0));
    }
    let Some(rest) = input.strip_prefix("---") else {
        return Ok((BTreeMap::new(), input, 0));
    };
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return Err(InputIssue::invalid("unclosed YAML frontmatter"));
    };
    let yaml = &rest[..end];
    let after = &rest[end + "\n---".len()..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    let map = serde_yaml::from_str::<BTreeMap<String, Value>>(yaml)
        .map_err(|error| InputIssue::invalid(error.to_string()).at("frontmatter"))?;
    let body_offset = input.len() - body.len();
    Ok((map, body, body_offset))
}

fn parse_blocks_with_spans(input: &str, base_offset: usize) -> InputResult<Vec<MarkdownBlockSpan>> {
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    if !input.contains(BLOCK_MARKER) {
        let body = input.trim().to_string();
        let fields = BTreeMap::from([("source".into(), Value::String("system".into()))]);
        return Ok(vec![MarkdownBlockSpan {
            block: MarkdownBlock {
                header: MarkdownHeader {
                    type_name: default_block_type(),
                    length: body.len(),
                    fields,
                },
                body,
            },
            start: base_offset,
            end: base_offset + input.len(),
        }]);
    }
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let mut blocks = Vec::new();
    while pos < bytes.len() {
        pos = skip_blank_lines(bytes, pos);
        if pos >= bytes.len() {
            break;
        }
        let line_end = bytes[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| pos + i)
            .unwrap_or(bytes.len());
        let line = std::str::from_utf8(&bytes[pos..line_end])
            .map_err(|e| InputIssue::invalid(format!("agenticmd utf8: {e}")))?;
        if !line.trim_start().starts_with(BLOCK_MARKER) {
            return Err(InputIssue::invalid(format!(
                "expected `{BLOCK_MARKER}:{{speaker}} {{json}} -->` at offset {}",
                base_offset + pos
            )));
        }
        let start = base_offset + pos;
        let (mut header, declared_length) = parse_block_comment(line.trim())?;
        let mut next = if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };
        next = if declared_length.is_some() {
            consume_one_line_break(bytes, next)
        } else {
            skip_blank_lines(bytes, next)
        };
        let body_end = declared_length
            .map(|length| next + length)
            .unwrap_or_else(|| {
                input[next..]
                    .find(BLOCK_MARKER)
                    .map(|offset| next + offset)
                    .unwrap_or(bytes.len())
            });
        if body_end > bytes.len() {
            return Err(InputIssue::invalid(format!(
                "agenticmd block body past EOF (need {} bytes)",
                declared_length.unwrap_or_default()
            )));
        }
        let raw_body = std::str::from_utf8(&bytes[next..body_end])
            .map_err(|e| InputIssue::invalid(format!("agenticmd body utf8: {e}")))?;
        let body = if declared_length.is_some() {
            raw_body.to_string()
        } else {
            raw_body.trim_end_matches(['\r', '\n']).to_string()
        };
        header.length = body.len();
        let end = base_offset + skip_blank_lines(bytes, body_end);
        blocks.push(MarkdownBlockSpan {
            block: MarkdownBlock { header, body },
            start,
            end,
        });
        pos = skip_blank_lines(bytes, body_end);
    }
    Ok(blocks)
}

fn parse_block_comment(line: &str) -> InputResult<(MarkdownHeader, Option<usize>)> {
    let after = line
        .strip_prefix(BLOCK_MARKER)
        .ok_or_else(|| InputIssue::invalid("missing persisting:block marker"))?;
    let after = after.strip_prefix(':').unwrap_or(after).trim_start();
    let json_start = after.find('{');
    let speaker = json_start
        .map(|i| after[..i].trim())
        .unwrap_or_else(|| after.strip_suffix("-->").unwrap_or(after).trim());
    let Some(json_start) = json_start else {
        let mut fields = BTreeMap::new();
        if !speaker.is_empty() {
            fields.insert(
                "source".into(),
                Value::String(normalize_source(speaker).into()),
            );
        }
        return Ok((
            MarkdownHeader {
                type_name: default_block_type(),
                length: 0,
                fields,
            },
            None,
        ));
    };
    let after = &after[json_start..];
    let json_part = after
        .strip_suffix("-->")
        .ok_or_else(|| InputIssue::invalid("unclosed block comment"))?
        .trim();
    let json_start = json_part
        .find('{')
        .ok_or_else(|| InputIssue::invalid("block JSON object missing"))?;
    let json_str = extract_json_object(&json_part[json_start..])?;
    let raw: Value =
        serde_json::from_str(json_str).map_err(|error| InputIssue::invalid(error.to_string()))?;
    let declared_length = raw
        .get("length")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let mut header: MarkdownHeader =
        serde_json::from_value(raw).map_err(|error| InputIssue::invalid(error.to_string()))?;
    if !speaker.is_empty()
        && !header.fields.contains_key("source")
        && !header.fields.contains_key("role")
    {
        header.fields.insert(
            "source".into(),
            Value::String(normalize_source(speaker).into()),
        );
    }
    Ok((header, declared_length))
}

fn default_block_type() -> String {
    "text".into()
}

fn normalize_source(speaker: &str) -> &str {
    match speaker {
        "assistant" | "agent" => "agent",
        "user" => "user",
        _ => "system",
    }
}

fn extract_json_object(s: &str) -> InputResult<&str> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return Err(InputIssue::invalid("expected JSON object"));
    }
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    Err(InputIssue::invalid(
        "unbalanced JSON object in block header",
    ))
}

fn skip_blank_lines(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() {
        if bytes[pos] == b'\n' {
            pos += 1;
            continue;
        }
        if bytes[pos] == b'\r' {
            pos += 1;
            continue;
        }
        break;
    }
    pos
}

fn consume_one_line_break(bytes: &[u8], pos: usize) -> usize {
    if bytes.get(pos..pos.saturating_add(2)) == Some(b"\r\n") {
        pos + 2
    } else if bytes.get(pos) == Some(&b'\n') {
        pos + 1
    } else {
        pos
    }
}

#[cfg(test)]
mod strict_frontmatter_tests {
    use std::collections::BTreeMap;

    use super::{encode_agenticmd_block, parse_agenticmd_document, MarkdownBlock, MarkdownHeader};

    #[test]
    fn malformed_agenticmd_yaml_is_not_silently_replaced() {
        let error = parse_agenticmd_document("---\nformat: [\n---\n\n").unwrap_err();
        assert_eq!(error.location(), Some("frontmatter"));
    }

    #[test]
    fn declared_length_preserves_leading_newlines_and_adjacent_blocks() {
        let block = |body: &str| MarkdownBlock {
            header: MarkdownHeader {
                type_name: "text".into(),
                length: 0,
                fields: BTreeMap::from([(
                    "source".into(),
                    serde_json::Value::String("user".into()),
                )]),
            },
            body: body.into(),
        };
        let expected = [block("\nleading-lf"), block("\r\nleading-crlf"), block("")];
        let encoded = expected
            .iter()
            .map(encode_agenticmd_block)
            .collect::<crate::Result<String>>()
            .unwrap();

        let decoded = parse_agenticmd_document(&encoded).unwrap();
        assert_eq!(
            decoded
                .blocks
                .iter()
                .map(|block| block.body.as_str())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|block| block.body.as_str())
                .collect::<Vec<_>>()
        );
    }
}
