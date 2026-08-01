//! `agenticmd` format — persisting-gateway TLV markdown dialogue view.
//!
//! On-disk layout matches capture `{session_id}.md`:
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
    #[serde(rename = "type")]
    pub type_name: String,
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
    pub fn role(&self) -> Option<&str> {
        self.header.fields.get("role").and_then(|v| v.as_str())
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
    pub frontmatter: BTreeMap<String, String>,
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
    parse_agenticmd_document_with(input, AgenticmdParseMode::Lenient)
}

/// Parse agenticmd with an explicit [`AgenticmdParseMode`].
pub fn parse_agenticmd_document_with(
    input: &str,
    mode: AgenticmdParseMode,
) -> Result<AgenticmdDocument> {
    let (frontmatter, _body, _off) = split_frontmatter_with_offset(input)?;
    let spans = parse_agenticmd_blocks_with_spans(input, mode)?;
    let mut doc = AgenticmdDocument::new(spans.into_iter().map(|s| s.block).collect());
    doc.frontmatter = frontmatter;
    if let Some(fmt) = doc.frontmatter.get("format") {
        doc.frontmatter_format = fmt.clone();
    }
    doc.session_id = doc
        .frontmatter
        .get("session")
        .cloned()
        .or_else(|| doc.frontmatter.get("session_id").cloned());
    doc.agent_id = doc
        .frontmatter
        .get("agent")
        .cloned()
        .or_else(|| doc.frontmatter.get("agent_id").cloned());
    Ok(doc)
}

/// How to treat non-block lines while parsing agenticmd.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgenticmdParseMode {
    /// Skip non-block lines (legacy notes). Used by `traj convert`.
    #[default]
    Lenient,
    /// Reject unexpected non-block content (capture live-document semantics).
    Strict,
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
pub fn parse_agenticmd_blocks_with_spans(
    input: &str,
    mode: AgenticmdParseMode,
) -> Result<Vec<AgenticmdBlockSpan>> {
    let (_frontmatter, body, body_offset) = split_frontmatter_with_offset(input)?;
    parse_blocks_with_spans(body, body_offset, mode)
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
    let mut out = String::from("---\n");
    out.push_str(&format!("format: {}\n", doc.frontmatter_format));
    if let Some(session) = &doc.session_id {
        out.push_str(&format!("session: {session}\n"));
    }
    if let Some(agent) = &doc.agent_id {
        out.push_str(&format!("agent: {agent}\n"));
    }
    for (k, v) in &doc.frontmatter {
        if matches!(
            k.as_str(),
            "format" | "session" | "session_id" | "agent" | "agent_id"
        ) {
            continue;
        }
        out.push_str(&format!("{k}: {v}\n"));
    }
    out.push_str("---\n\n");
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
    let speaker = header
        .fields
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("note");
    let json = serde_json::to_string(&header)?;
    Ok(format!(
        "{BLOCK_MARKER}:{speaker} {json} -->\n\n{}\n\n",
        block.body
    ))
}

fn split_frontmatter_with_offset(input: &str) -> Result<(BTreeMap<String, String>, &str, usize)> {
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
    let mut map = BTreeMap::new();
    for line in yaml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let body_offset = input.len() - body.len();
    Ok((map, body, body_offset))
}

fn parse_blocks_with_spans(
    input: &str,
    base_offset: usize,
    mode: AgenticmdParseMode,
) -> Result<Vec<AgenticmdBlockSpan>> {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let mut blocks = Vec::new();
    while pos < bytes.len() {
        pos = match mode {
            AgenticmdParseMode::Lenient => skip_ws(bytes, pos),
            AgenticmdParseMode::Strict => skip_strict_preamble(bytes, pos),
        };
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
            match mode {
                AgenticmdParseMode::Lenient => {
                    pos = if line_end < bytes.len() {
                        line_end + 1
                    } else {
                        line_end
                    };
                    continue;
                }
                AgenticmdParseMode::Strict => {
                    return Err(Error::Other(format!(
                        "expected `{BLOCK_MARKER}:{{speaker}} {{json}} -->` at offset {}",
                        base_offset + pos
                    )));
                }
            }
        }
        let start = base_offset + pos;
        let header: AgenticmdHeader = parse_block_comment(line.trim())?;
        let mut next = if line_end < bytes.len() {
            line_end + 1
        } else {
            line_end
        };
        next = skip_blank_lines(bytes, next);
        let body_end = next + header.length;
        if body_end > bytes.len() {
            return Err(Error::Other(format!(
                "agenticmd block body past EOF (need {} bytes)",
                header.length
            )));
        }
        let body = std::str::from_utf8(&bytes[next..body_end])
            .map_err(|e| Error::Other(format!("agenticmd body utf8: {e}")))?
            .to_string();
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

fn parse_block_comment(line: &str) -> Result<AgenticmdHeader> {
    let after = line
        .strip_prefix(BLOCK_MARKER)
        .ok_or_else(|| Error::Other("missing persisting:block marker".into()))?
        .trim_start();
    let after = if let Some(rest) = after.strip_prefix(':') {
        let json_start = rest
            .find('{')
            .ok_or_else(|| Error::Other("block JSON object missing".into()))?;
        &rest[json_start..]
    } else {
        after
    };
    let json_part = after
        .strip_suffix("-->")
        .ok_or_else(|| Error::Other("unclosed block comment".into()))?
        .trim();
    let json_start = json_part
        .find('{')
        .ok_or_else(|| Error::Other("block JSON object missing".into()))?;
    let json_str = extract_json_object(&json_part[json_start..])?;
    Ok(serde_json::from_str(json_str)?)
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

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\r' | b'\n') {
        pos += 1;
    }
    pos
}

/// Capture-compatible preamble skip: blank lines and `#` comment lines only.
fn skip_strict_preamble(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() {
        if bytes[pos] == b'#' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            if pos < bytes.len() {
                pos += 1;
            }
            continue;
        }
        if bytes[pos] == b'\n' || bytes[pos] == b'\r' {
            pos = skip_blank_lines(bytes, pos);
            continue;
        }
        break;
    }
    pos
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
