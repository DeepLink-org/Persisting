//! `agenticmd` — best-effort Markdown view for humans and debugging.
//!
//! It is intentionally not a canonical storage format. New writers use
//! Storyline-like identity fields (`session_id`, `agent_id`, `source`,
//! `step_id`); readers retain aliases for older capture documents.
//! ```text
//! ---
//! format: persisting:1.0   # logical name in pChronicle: agenticmd
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

use crate::{Error, Result};

pub const AGENTICMD_FORMAT_NAME: &str = "agenticmd";
pub const AGENTICMD_FRONTMATTER_FORMAT: &str = "persisting:1.0";
pub const BLOCK_MARKER: &str = "<!-- persisting:block";
/// Layout hint embedded in capture live-document YAML (`block:` field).
pub const AGENTICMD_BLOCK_LAYOUT: &str =
    "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgenticmdHeader {
    #[serde(rename = "type", default = "default_block_type")]
    pub type_name: String,
    #[serde(default)]
    pub length: usize,
    #[serde(flatten)]
    pub fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgenticmdBlock {
    pub header: AgenticmdHeader,
    pub body: String,
}

impl AgenticmdBlock {
    /// Legacy presentation role, derived from Storyline `source` when absent.
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

    pub fn step_id(&self) -> Option<i64> {
        ["step_id", "id", "seq"]
            .iter()
            .find_map(|key| self.header.fields.get(*key).and_then(|v| v.as_i64()))
    }

    pub fn kind(&self) -> Option<&str> {
        self.header.fields.get("kind").and_then(|v| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgenticmdDocument {
    /// Logical pChronicle format name (`agenticmd`).
    pub format: String,
    /// Frontmatter `format:` value (usually `persisting:1.0`).
    pub frontmatter_format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub frontmatter: BTreeMap<String, Value>,
    pub blocks: Vec<AgenticmdBlock>,
}

impl AgenticmdDocument {
    pub fn new(blocks: Vec<AgenticmdBlock>) -> Self {
        Self {
            format: AGENTICMD_FORMAT_NAME.into(),
            frontmatter_format: AGENTICMD_FRONTMATTER_FORMAT.into(),
            session_id: None,
            agent_id: None,
            frontmatter: BTreeMap::new(),
            blocks,
        }
    }
}

pub fn parse_agenticmd_document(input: &str) -> Result<AgenticmdDocument> {
    let (frontmatter, _body, _off) = split_frontmatter_with_offset(input)?;
    let spans = parse_agenticmd_blocks_with_spans(input)?;
    let mut doc = AgenticmdDocument::new(spans.into_iter().map(|s| s.block).collect());
    doc.frontmatter = frontmatter;
    if let Some(fmt) = doc.frontmatter.get("format").and_then(Value::as_str) {
        doc.frontmatter_format = fmt.to_string();
    }
    doc.session_id = doc
        .frontmatter
        .get("session_id")
        .or_else(|| doc.frontmatter.get("session"))
        .and_then(Value::as_str)
        .map(str::to_string);
    doc.agent_id = doc
        .frontmatter
        .get("agent_id")
        .and_then(Value::as_str)
        .or_else(|| doc.frontmatter.get("agent").and_then(Value::as_str))
        .or_else(|| {
            doc.frontmatter
                .get("agent")
                .and_then(Value::as_object)
                .and_then(|agent| agent.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    Ok(doc)
}

/// One parsed block plus its absolute byte range in the source document.
///
/// `start..end` covers the comment line through trailing blank lines — the same
/// span capture uses for markdown upsert rewrites.
#[derive(Debug, Clone, PartialEq)]
pub struct AgenticmdBlockSpan {
    pub block: AgenticmdBlock,
    pub start: usize,
    pub end: usize,
}

/// Parse blocks with absolute byte spans (for capture upsert / diagnostics).
pub fn parse_agenticmd_blocks_with_spans(input: &str) -> Result<Vec<AgenticmdBlockSpan>> {
    let (_frontmatter, body, body_offset) = split_frontmatter_with_offset(input)?;
    parse_blocks_with_spans(body, body_offset)
}

/// Byte offset where the document body begins (immediately after YAML frontmatter).
///
/// Returns `0` when there is no opening `---` frontmatter. Errors on unclosed
/// frontmatter. Does **not** skip `#` comment lines — callers that rewrite
/// session rollup frontmatter should preserve everything after this offset.
pub fn agenticmd_body_byte_offset(input: &str) -> Result<usize> {
    let (_fm, _body, offset) = split_frontmatter_with_offset(input)?;
    Ok(offset)
}

/// Encode a YAML frontmatter fence (`---\n…\n---\n\n`) from any serializable mapping.
///
/// Used by capture for live document / session-rollup preambles (nested `client`, etc.).
/// Distinct from [`encode_agenticmd_document`], which emits a flat string frontmatter
/// suitable for hub interchange.
pub fn encode_agenticmd_preamble<T: Serialize>(frontmatter: &T) -> Result<String> {
    let yaml = serde_yaml::to_string(frontmatter)
        .map_err(|e| Error::Other(format!("agenticmd frontmatter yaml: {e}")))?;
    Ok(format!("---\n{yaml}---\n\n"))
}

pub fn encode_agenticmd_document(doc: &AgenticmdDocument) -> Result<String> {
    let mut frontmatter = doc.frontmatter.clone();
    frontmatter.insert(
        "format".into(),
        Value::String(doc.frontmatter_format.clone()),
    );
    if let Some(session) = &doc.session_id {
        frontmatter.insert("session_id".into(), Value::String(session.clone()));
    }
    if let Some(agent) = &doc.agent_id {
        frontmatter.insert("agent_id".into(), Value::String(agent.clone()));
    }
    let mut out = encode_agenticmd_preamble(&frontmatter)?;
    for block in &doc.blocks {
        out.push_str(&encode_agenticmd_block(block)?);
    }
    Ok(out)
}

/// Encode a single agenticmd / capture TLV block (comment header + body).
///
/// Normative on-disk layout shared with `persisting-gateway` live write paths.
pub fn encode_agenticmd_block(block: &AgenticmdBlock) -> Result<String> {
    let header = AgenticmdHeader {
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

fn split_frontmatter_with_offset(input: &str) -> Result<(BTreeMap<String, Value>, &str, usize)> {
    if !input.starts_with("---") {
        return Ok((BTreeMap::new(), input, 0));
    }
    let Some(rest) = input.strip_prefix("---") else {
        return Ok((BTreeMap::new(), input, 0));
    };
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return Err(Error::Other("unclosed YAML frontmatter".into()));
    };
    let yaml = &rest[..end];
    let after = &rest[end + "\n---".len()..];
    let body = after.strip_prefix('\n').unwrap_or(after);
    let map = serde_yaml::from_str::<BTreeMap<String, Value>>(yaml).unwrap_or_default();
    let body_offset = input.len() - body.len();
    Ok((map, body, body_offset))
}

fn parse_blocks_with_spans(input: &str, base_offset: usize) -> Result<Vec<AgenticmdBlockSpan>> {
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    if !input.contains(BLOCK_MARKER) {
        let body = input.trim().to_string();
        let fields = BTreeMap::from([("source".into(), Value::String("system".into()))]);
        return Ok(vec![AgenticmdBlockSpan {
            block: AgenticmdBlock {
                header: AgenticmdHeader {
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
            .map_err(|e| Error::Other(format!("agenticmd utf8: {e}")))?;
        if !line.trim_start().starts_with(BLOCK_MARKER) {
            return Err(Error::Other(format!(
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
        next = skip_blank_lines(bytes, next);
        let body_end = declared_length
            .map(|length| next + length)
            .unwrap_or_else(|| {
                input[next..]
                    .find(BLOCK_MARKER)
                    .map(|offset| next + offset)
                    .unwrap_or(bytes.len())
            });
        if body_end > bytes.len() {
            return Err(Error::Other(format!(
                "agenticmd block body past EOF (need {} bytes)",
                declared_length.unwrap_or_default()
            )));
        }
        let raw_body = std::str::from_utf8(&bytes[next..body_end])
            .map_err(|e| Error::Other(format!("agenticmd body utf8: {e}")))?;
        let body = if declared_length.is_some() {
            raw_body.to_string()
        } else {
            raw_body.trim_end_matches(['\r', '\n']).to_string()
        };
        header.length = body.len();
        let end = base_offset + skip_blank_lines(bytes, body_end);
        blocks.push(AgenticmdBlockSpan {
            block: AgenticmdBlock { header, body },
            start,
            end,
        });
        pos = skip_blank_lines(bytes, body_end);
    }
    Ok(blocks)
}

fn parse_block_comment(line: &str) -> Result<(AgenticmdHeader, Option<usize>)> {
    let after = line
        .strip_prefix(BLOCK_MARKER)
        .ok_or_else(|| Error::Other("missing persisting:block marker".into()))?;
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
            AgenticmdHeader {
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
        .ok_or_else(|| Error::Other("unclosed block comment".into()))?
        .trim();
    let json_start = json_part
        .find('{')
        .ok_or_else(|| Error::Other("block JSON object missing".into()))?;
    let json_str = extract_json_object(&json_part[json_start..])?;
    let raw: Value = serde_json::from_str(json_str)?;
    let declared_length = raw
        .get("length")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let mut header: AgenticmdHeader = serde_json::from_value(raw)?;
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

fn extract_json_object(s: &str) -> Result<&str> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'{') {
        return Err(Error::Other("expected JSON object".into()));
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
    Err(Error::Other(
        "unbalanced JSON object in block header".into(),
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
