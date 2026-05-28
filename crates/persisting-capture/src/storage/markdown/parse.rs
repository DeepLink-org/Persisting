//! Parse `<!-- persisting:block -->` documents.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::types::{BlockHeader, MarkdownBlock, BLOCK_MARKER};

pub fn parse_document(input: &str) -> Result<Vec<MarkdownBlock>> {
    let bytes = input.as_bytes();
    let mut pos = skip_preamble(bytes, 0)?;
    let mut blocks = Vec::new();

    while pos < bytes.len() {
        pos = skip_preamble(bytes, pos)?;
        if pos >= bytes.len() {
            break;
        }
        let (header, body, next) = parse_one_block(bytes, pos)?;
        blocks.push(MarkdownBlock { header, body });
        pos = next;
    }
    Ok(blocks)
}

pub fn read_blocks_from_file(path: &Path) -> Result<Vec<MarkdownBlock>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut s = String::new();
    file.read_to_string(&mut s)?;
    parse_document(&s)
}

pub fn block_count(path: &Path) -> Result<usize> {
    Ok(read_blocks_from_file(path)?.len())
}
pub(crate) fn parse_one_block(bytes: &[u8], pos: usize) -> Result<(BlockHeader, Vec<u8>, usize)> {
    let line = line_at(bytes, pos).with_context(|| format!("block at offset {pos}"))?;
    if !line.trim_start().starts_with(BLOCK_MARKER) {
        bail!("expected `{BLOCK_MARKER}:{{speaker}} {{json}} -->` at offset {pos}");
    }
    let header = parse_block_comment(line)?;
    validate_type_name(&header.type_name)?;

    let mut next = pos + line.len();
    if next < bytes.len() && bytes[next] == b'\n' {
        next += 1;
    }

    let body_start = skip_blank_lines(bytes, next);
    let body_end = body_start + header.length;
    if body_end > bytes.len() {
        bail!(
            "block body past EOF (type={}, need {} bytes)",
            header.type_name,
            header.length
        );
    }
    let body = bytes[body_start..body_end].to_vec();
    // Include trailing blank lines from [`encode_block_with_header`] (`\n\n` after body).
    // Omitting them causes upsert to leave old separators behind and accumulate empty lines.
    let end = skip_blank_lines(bytes, body_end);
    Ok((header, body, end))
}

fn parse_block_comment(line: &str) -> Result<BlockHeader> {
    let line = line.trim();
    let after = line
        .strip_prefix(BLOCK_MARKER)
        .context("persisting:block marker")?
        .trim_start();
    let after = if let Some(rest) = after.strip_prefix(':') {
        let json_start = rest.find('{').context("block JSON object")?;
        let speaker = rest[..json_start].trim();
        validate_speaker(speaker)?;
        &rest[json_start..]
    } else {
        after
    };
    let json_part = after
        .strip_suffix("-->")
        .context("unclosed block comment")?
        .trim();
    let json_start = json_part.find('{').context("block JSON object")?;
    let json_str = extract_json_object(&json_part[json_start..])?;
    serde_json::from_str(json_str).context("parse block JSON")
}

pub(crate) fn block_speaker(header: &BlockHeader) -> &str {
    header
        .fields
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("note")
}

pub(crate) fn validate_speaker(speaker: &str) -> Result<()> {
    let s = speaker.trim();
    if s.is_empty() {
        bail!("block speaker must not be empty");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        bail!("invalid block speaker: {speaker}");
    }
    Ok(())
}

fn extract_json_object(s: &str) -> Result<&str> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in s.char_indices() {
        if in_string {
            if escape {
                escape = false;
                continue;
            }
            if ch == '\\' {
                escape = true;
                continue;
            }
            if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    bail!("unclosed JSON in block comment");
}

pub(crate) fn validate_type_name(type_name: &str) -> Result<()> {
    let t = type_name.trim();
    if t.is_empty() {
        bail!("block type must not be empty");
    }
    if t.contains('\n') || t.contains(':') {
        bail!("block type must not contain ':' or newline");
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/'))
    {
        bail!("invalid block type: {type_name}");
    }
    Ok(())
}

pub(crate) fn skip_preamble(bytes: &[u8], mut pos: usize) -> Result<usize> {
    pos = skip_yaml_frontmatter(bytes, pos)?;
    while pos < bytes.len() {
        if bytes[pos] == b'#' {
            let line_end = bytes[pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|i| pos + i + 1)
                .unwrap_or(bytes.len());
            pos = line_end;
            continue;
        }
        if bytes[pos] == b'\n' || bytes[pos] == b'\r' {
            pos = advance_newline(bytes, pos);
            continue;
        }
        break;
    }
    Ok(pos)
}

/// Skip an opening `---` YAML frontmatter block (if present).
fn skip_yaml_frontmatter(bytes: &[u8], pos: usize) -> Result<usize> {
    let Some(line) = line_at(bytes, pos) else {
        return Ok(pos);
    };
    if line.trim() != "---" {
        return Ok(pos);
    }

    let mut cursor = pos + line.len();
    cursor = advance_newline(bytes, cursor);

    while cursor < bytes.len() {
        let Some(inner) = line_at(bytes, cursor) else {
            break;
        };
        if inner.trim() == "---" {
            cursor += inner.len();
            cursor = advance_newline(bytes, cursor);
            return Ok(cursor);
        }
        cursor += inner.len();
        cursor = advance_newline(bytes, cursor);
    }
    bail!("unclosed YAML frontmatter");
}

fn advance_newline(bytes: &[u8], mut pos: usize) -> usize {
    if pos >= bytes.len() {
        return pos;
    }
    if bytes[pos] == b'\r' {
        pos += 1;
        if pos < bytes.len() && bytes[pos] == b'\n' {
            pos += 1;
        }
        return pos;
    }
    if bytes[pos] == b'\n' {
        pos += 1;
    }
    pos
}

fn skip_blank_lines(bytes: &[u8], mut pos: usize) -> usize {
    while let Some(line) = line_at(bytes, pos) {
        if line.trim().is_empty() {
            pos += line.len();
            if pos < bytes.len() && bytes[pos] == b'\n' {
                pos += 1;
            }
        } else {
            break;
        }
    }
    pos
}

fn line_at(bytes: &[u8], pos: usize) -> Option<&str> {
    if pos >= bytes.len() {
        return None;
    }
    let rest = &bytes[pos..];
    match rest.iter().position(|&b| b == b'\n') {
        Some(n) => std::str::from_utf8(&rest[..n]).ok(),
        None => std::str::from_utf8(rest).ok(),
    }
}
