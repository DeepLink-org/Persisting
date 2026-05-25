//! Session markdown (`{session_id}.md`): `<!-- persisting:block:{speaker} {json} -->` + body.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::dialogue::{block_to_replay_json, try_capture_record_to_block};
use super::session_client::{resolve_client_meta_for_run_dir, SessionClientMeta};

/// Legacy numbered session file (pre `{session_id}.md` layout).
pub const SESSION_MARKDOWN_FILENAME: &str = "0001.md";

/// Max filename stem length for `{session_id}.md` on disk.
const SESSION_FILENAME_MAX_LEN: usize = 128;

/// Legacy filename; still recognized for read / import.
pub const LEGACY_TRAJECTORY_MARKDOWN_FILENAME: &str = "trajectory.tlv.md";

const BLOCK_MARKER: &str = "<!-- persisting:block";
pub(crate) const BLOCK_LAYOUT: &str = "<!-- persisting:block:{speaker} {json} -->\n\nmessage body\n\n";

#[derive(Serialize)]
struct DocumentFrontmatter<'a> {
    format: &'static str,
    block: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<&'a SessionClientMeta>,
}

/// Build YAML frontmatter; includes detected client process when available.
pub fn format_document_preamble(client: Option<&SessionClientMeta>) -> Result<String> {
    let doc = DocumentFrontmatter {
        format: "persisting:1.0",
        block: BLOCK_LAYOUT,
        client,
    };
    let yaml = serde_yaml::to_string(&doc).context("serialize trajectory frontmatter")?;
    Ok(format!("---\n{yaml}---\n\n"))
}

/// Human-readable duration for frontmatter (`42s`, `3m12s`, `1h5m`).
pub fn format_duration_human(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{mins}m")
    }
}

/// Byte offset in `content` where the first trajectory block begins (after YAML preamble).
pub fn document_body_start(content: &str) -> Result<usize> {
    skip_preamble(content.as_bytes(), 0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    #[serde(rename = "type")]
    pub type_name: String,
    pub length: usize,
    #[serde(flatten)]
    pub fields: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct MarkdownBlock {
    pub header: BlockHeader,
    pub body: Vec<u8>,
}

impl MarkdownBlock {
    pub fn type_name(&self) -> &str {
        &self.header.type_name
    }

    pub fn value_utf8(&self) -> Result<&str> {
        std::str::from_utf8(&self.body).context("block body must be UTF-8")
    }

    pub fn role(&self) -> Option<&str> {
        self.header.fields.get("role").and_then(|v| v.as_str())
    }

    pub fn kind(&self) -> Option<&str> {
        self.header.fields.get("kind").and_then(|v| v.as_str())
    }

    pub fn turn(&self) -> Option<u64> {
        self.header.fields.get("turn").and_then(|v| v.as_u64())
    }
}

/// Sanitize a logical session id for use as a markdown filename stem.
pub fn sanitize_session_filename(session_id: &str) -> String {
    let trimmed = session_id.trim();
    let mut out = String::with_capacity(trimmed.len().min(SESSION_FILENAME_MAX_LEN));
    for c in trimmed.chars() {
        if out.len() >= SESSION_FILENAME_MAX_LEN {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "session".to_string()
    } else {
        out
    }
}

/// Markdown filename for a logical session (`{session_id}.md`).
pub fn session_markdown_filename(session_key: &str) -> String {
    format!("{}.md", sanitize_session_filename(session_key))
}

pub fn session_markdown_path(session_dir: &Path) -> PathBuf {
    session_dir.join(SESSION_MARKDOWN_FILENAME)
}

/// `{run_dir}/{session_key}.md`
pub fn session_markdown_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    run_dir.join(session_markdown_filename(session_key))
}

/// Path to append markdown blocks for one session key under a run directory.
pub fn session_markdown_write_path_for_key(run_dir: &Path, session_key: &str) -> PathBuf {
    locate_session_markdown_for_key(run_dir, session_key)
        .unwrap_or_else(|| session_markdown_path_for_key(run_dir, session_key))
}

/// Path to append markdown blocks: existing session file, or [`SESSION_MARKDOWN_FILENAME`] for new sessions.
pub fn session_markdown_write_path(session_dir: &Path) -> PathBuf {
    locate_session_markdown(session_dir).unwrap_or_else(|| session_markdown_path(session_dir))
}

/// Whether `session_key` names a subagent markdown stem (`agent-{id}`).
pub fn is_subagent_session_storage_key(session_key: &str) -> bool {
    sanitize_session_filename(session_key)
        .strip_prefix("agent-")
        .is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

fn is_subagent_markdown_filename(name: &str) -> bool {
    name.strip_suffix(".md")
        .is_some_and(|stem| is_subagent_session_storage_key(stem))
}

/// Capture-run bucket markdown: `{run_dir}/run-{id}.md` when the directory itself is `run-*`.
pub fn locate_run_bucket_markdown(run_dir: &Path) -> Option<PathBuf> {
    let stem = run_dir.file_name()?.to_str()?;
    if !stem.starts_with("run-") {
        return None;
    }
    let path = run_dir.join(session_markdown_filename(stem));
    path.is_file().then_some(path)
}

/// Find markdown for one session key under a run directory (prefers `{key}.md`, then legacy `0001.md`).
pub fn locate_session_markdown_for_key(run_dir: &Path, session_key: &str) -> Option<PathBuf> {
    let named = session_markdown_path_for_key(run_dir, session_key);
    if named.is_file() {
        return Some(named);
    }
    // Subagent keys always map to sibling `agent-{id}.md`; never inherit the main run file.
    if is_subagent_session_storage_key(session_key) {
        return None;
    }
    // Main session in a capture run: prefer the run bucket file over subagent siblings.
    if let Some(run_md) = locate_run_bucket_markdown(run_dir) {
        return Some(run_md);
    }
    locate_session_markdown(run_dir)
}

/// Find the readable session markdown file (legacy `0001.md`, `{session_id}.md`, …).
pub fn locate_session_markdown(session_dir: &Path) -> Option<PathBuf> {
    let preferred = session_dir.join(SESSION_MARKDOWN_FILENAME);
    if preferred.is_file() {
        return Some(preferred);
    }
    let legacy = session_dir.join(LEGACY_TRAJECTORY_MARKDOWN_FILENAME);
    if legacy.is_file() {
        return Some(legacy);
    }
    let mut session_key_files = Vec::new();
    let mut numbered = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(session_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if is_numbered_session_markdown_name(name) {
                numbered.push(path);
            } else if is_session_key_markdown_name(name) && !is_subagent_markdown_filename(name) {
                session_key_files.push(path);
            }
        }
    }
    session_key_files.sort();
    if let Some(path) = session_key_files.into_iter().next() {
        return Some(path);
    }
    numbered.sort();
    numbered.into_iter().next()
}

/// Whether `path` names a persisting session markdown file (`{session_id}.md`, `0001.md`, …).
pub fn is_trajectory_markdown_path(path: impl AsRef<Path>) -> bool {
    let Some(name) = path.as_ref().file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    is_numbered_session_markdown_name(name)
        || is_session_key_markdown_name(name)
        || name.eq_ignore_ascii_case(LEGACY_TRAJECTORY_MARKDOWN_FILENAME)
        || name.to_ascii_lowercase().ends_with(".tlv.md")
}

fn is_session_key_markdown_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    if stem.is_empty()
        || is_numbered_session_markdown_name(name)
        || name.eq_ignore_ascii_case(LEGACY_TRAJECTORY_MARKDOWN_FILENAME)
        || name.to_ascii_lowercase().ends_with(".tlv.md")
    {
        return false;
    }
    stem.starts_with("agent-")
        || stem.starts_with("run-")
        || (stem.contains('-') && stem.len() >= 8)
}

fn is_numbered_session_markdown_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    stem.len() == 4 && stem.chars().all(|c| c.is_ascii_digit())
}

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

pub fn append_engine_lines_to_markdown(
    path: &Path,
    engine_lines: &[impl AsRef<str>],
) -> Result<usize> {
    if engine_lines.is_empty() {
        return Ok(0);
    }
    let mut blocks = Vec::new();
    for line in engine_lines {
        let rec = crate::record::engine_line_to_record(line.as_ref())?;
        if let Some(block) = try_capture_record_to_block(&rec)? {
            blocks.push(block);
        }
    }
    write_blocks(path, &blocks)
}

pub fn encode_block_with_header(header: BlockHeader, body: &[u8]) -> Result<String> {
    validate_type_name(&header.type_name)?;
    std::str::from_utf8(body).context("block body must be UTF-8")?;
    let mut header = header;
    header.length = body.len();
    let json = serde_json::to_string(&header).context("serialize block JSON")?;
    let body = std::str::from_utf8(body).unwrap();
    let speaker = block_speaker(&header);
    validate_speaker(speaker)?;
    Ok(format!("{BLOCK_MARKER}:{speaker} {json} -->\n\n{body}\n\n"))
}

pub fn replay_json_lines(
    blocks: &[MarkdownBlock],
    offset: usize,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    let end = limit
        .map(|lim| (offset + lim).min(blocks.len()))
        .unwrap_or(blocks.len());
    blocks
        .get(offset..end)
        .unwrap_or(&[])
        .iter()
        .map(block_to_replay_json)
        .collect()
}

fn write_blocks(path: &Path, blocks: &[(BlockHeader, Vec<u8>)]) -> Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    if file.metadata()?.len() == 0 {
        let client = path.parent().and_then(|run_dir| {
            run_dir
                .parent()
                .and_then(|agent_dir| agent_dir.parent())
                .and_then(|storage| resolve_client_meta_for_run_dir(storage, run_dir))
        });
        file.write_all(format_document_preamble(client.as_ref())?.as_bytes())?;
    }
    for (header, body) in blocks {
        file.write_all(encode_block_with_header(header.clone(), body)?.as_bytes())?;
    }
    file.sync_all().ok();
    Ok(blocks.len())
}

/// Replace the block whose header `call_id` and `role` match, or append when missing.
pub fn upsert_block_by_call_id(
    path: &Path,
    call_id: &str,
    block: (BlockHeader, Vec<u8>),
) -> Result<bool> {
    if call_id.trim().is_empty() {
        bail!("call_id must not be empty for markdown upsert");
    }
    let role = block
        .0
        .fields
        .get("role")
        .and_then(|v| v.as_str())
        .context("markdown upsert block missing role field")?;
    if !path.exists() {
        write_blocks(path, std::slice::from_ref(&block))?;
        return Ok(false);
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if let Some((start, end, _header)) = find_block_by_call_id_and_role(&bytes, call_id, role)? {
        let encoded = encode_block_with_header(block.0, &block.1)?;
        rewrite_block_range(path, start, end, encoded.as_bytes())?;
        Ok(true)
    } else {
        write_blocks(path, std::slice::from_ref(&block))?;
        Ok(false)
    }
}

fn find_block_by_call_id_and_role(
    bytes: &[u8],
    call_id: &str,
    role: &str,
) -> Result<Option<(usize, usize, BlockHeader)>> {
    let mut pos = skip_preamble(bytes, 0)?;
    while pos < bytes.len() {
        pos = skip_preamble(bytes, pos)?;
        if pos >= bytes.len() {
            break;
        }
        let start = pos;
        let (header, _body, end) = parse_one_block(bytes, pos)?;
        if block_matches_upsert_key(&header, call_id, role) {
            return Ok(Some((start, end, header)));
        }
        pos = end;
    }
    Ok(None)
}

fn block_matches_upsert_key(header: &BlockHeader, call_id: &str, role: &str) -> bool {
    header.fields.get("call_id").and_then(|v| v.as_str()) == Some(call_id)
        && header.fields.get("role").and_then(|v| v.as_str()) == Some(role)
}

/// Replace `[start, end)` with `new_block`, preserving bytes before `start` and after `end`.
fn rewrite_block_range(path: &Path, start: usize, end: usize, new_block: &[u8]) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if start > bytes.len() {
        bail!("block start {start} past EOF ({})", bytes.len());
    }
    if end > bytes.len() {
        bail!("block end {end} past EOF ({})", bytes.len());
    }
    if end < start {
        bail!("block end {end} before start {start}");
    }
    let mut out = bytes[..start].to_vec();
    out.extend_from_slice(new_block);
    out.extend_from_slice(&bytes[end..]);
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn parse_one_block(bytes: &[u8], pos: usize) -> Result<(BlockHeader, Vec<u8>, usize)> {
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

fn block_speaker(header: &BlockHeader) -> &str {
    header
        .fields
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("note")
}

fn validate_speaker(speaker: &str) -> Result<()> {
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

fn validate_type_name(type_name: &str) -> Result<()> {
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

fn skip_preamble(bytes: &[u8], mut pos: usize) -> Result<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::record_to_engine_line;
    use crate::sink::llm_request_record;

    fn baseline_preamble() -> String {
        format_document_preamble(None).unwrap()
    }

    #[test]
    fn body_may_contain_markdown_syntax() {
        let inner = b"---\n```rust\nfn main() {}\n```\n";
        let (h, b) = (
            BlockHeader {
                type_name: "markdown".into(),
                length: inner.len(),
                fields: BTreeMap::new(),
            },
            inner.to_vec(),
        );
        let doc = encode_block_with_header(h, &b).unwrap();
        assert_eq!(parse_document(&doc).unwrap()[0].body, inner);
    }

    #[test]
    fn block_comment_separated_from_body_by_blank_line() {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), serde_json::json!("user"));
        let encoded = encode_block_with_header(
            BlockHeader {
                type_name: "markdown".into(),
                length: 2,
                fields,
            },
            b"hi",
        )
        .unwrap();
        assert!(encoded.contains("-->\n\nhi\n\n"));
    }

    #[test]
    fn block_marker_includes_speaker() {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), serde_json::json!("user"));
        fields.insert("kind".into(), serde_json::json!("llm.request"));
        let encoded = encode_block_with_header(
            BlockHeader {
                type_name: "markdown".into(),
                length: 3,
                fields,
            },
            b"hey",
        )
        .unwrap();
        assert!(encoded.starts_with("<!-- persisting:block:user "));
        assert_eq!(parse_document(&encoded).unwrap().len(), 1);
    }

    #[test]
    fn parse_legacy_block_marker_without_speaker() {
        let doc = format!(
            "{BLOCK_MARKER} {{\"type\":\"markdown\",\"length\":2,\"role\":\"assistant\"}} -->\nok\n"
        );
        let blocks = parse_document(&doc).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].role(), Some("assistant"));
    }

    #[test]
    fn append_engine_line_writes_block_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_MARKDOWN_FILENAME);
        let rec = llm_request_record(
            None,
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"ping"}]}),
        );
        let line = record_to_engine_line(&rec).unwrap();
        append_engine_lines_to_markdown(&path, &[line.as_str()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(BLOCK_MARKER));
        assert!(text.contains("\"role\":\"user\""));
    }

    #[test]
    fn new_file_uses_yaml_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SESSION_MARKDOWN_FILENAME);
        let rec = llm_request_record(
            None,
            None,
            "m",
            "/v1",
            &serde_json::json!({"messages":[{"role":"user","content":"ping"}]}),
        );
        let line = record_to_engine_line(&rec).unwrap();
        append_engine_lines_to_markdown(&path, &[line.as_str()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\n"));
        assert!(text.contains("persisting:1.0"));
        assert!(text.contains("message body"));
        assert!(text.contains("persisting:block:{speaker}"));
        assert!(!text.starts_with("# "));
    }

    #[test]
    fn parse_legacy_hash_preamble() {
        let doc = format!(
            "# persisting trajectory\n# legacy header\n\n{}",
            encode_block_with_header(
                BlockHeader {
                    type_name: "markdown".into(),
                    length: 2,
                    fields: BTreeMap::new(),
                },
                b"hi",
            )
            .unwrap()
        );
        let blocks = parse_document(&doc).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].value_utf8().unwrap(), "hi");
    }

    #[test]
    fn numbered_session_markdown_detected() {
        assert!(is_trajectory_markdown_path("0001.md"));
        assert!(is_trajectory_markdown_path("/a/b/0042.md"));
        assert!(is_trajectory_markdown_path("trajectory.tlv.md"));
        assert!(is_trajectory_markdown_path("agent-abc123.md"));
        assert!(is_trajectory_markdown_path(
            "5e27e4a7-f42a-42a9-8448-79608bd95c53.md"
        ));
        assert!(!is_trajectory_markdown_path("notes.md"));
    }

    #[test]
    fn locate_prefers_0001_over_legacy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LEGACY_TRAJECTORY_MARKDOWN_FILENAME), "x").unwrap();
        std::fs::write(dir.path().join(SESSION_MARKDOWN_FILENAME), "y").unwrap();
        assert_eq!(
            locate_session_markdown(dir.path()).unwrap(),
            dir.path().join(SESSION_MARKDOWN_FILENAME)
        );
    }

    #[test]
    fn subagent_key_does_not_fallback_to_main_run_md() {
        let dir = tempfile::tempdir().unwrap();
        let main_md = dir.path().join("run-20260524-160709-170794000.md");
        std::fs::write(&main_md, "main").unwrap();
        let subagent_key = "agent-ad67e572475568b5a";
        assert!(is_subagent_session_storage_key(subagent_key));
        assert!(!is_subagent_session_storage_key(
            "37343ad1-ed7d-49dc-b080-9c4afd9873c2"
        ));
        assert_eq!(
            locate_session_markdown_for_key(dir.path(), subagent_key),
            None
        );
        assert_eq!(
            session_markdown_write_path_for_key(dir.path(), subagent_key),
            dir.path().join("agent-ad67e572475568b5a.md")
        );
        assert_eq!(
            locate_session_markdown_for_key(dir.path(), "37343ad1-ed7d-49dc-b080-9c4afd9873c2"),
            Some(main_md.clone())
        );
    }

    #[test]
    fn main_session_prefers_run_md_over_subagent_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run-20260524-161537-122998000");
        std::fs::create_dir_all(&run_dir).unwrap();
        let main_md = run_dir.join("run-20260524-161537-122998000.md");
        std::fs::write(&main_md, "main").unwrap();
        std::fs::write(run_dir.join("agent-a2560e716f0b8b526.md"), "sub").unwrap();
        std::fs::write(run_dir.join("agent-a0df18417539eecd0.md"), "sub2").unwrap();

        let header_session = "fb47835b-e10d-4b29-abc3-68f4594ebce3";
        assert_eq!(
            locate_session_markdown_for_key(&run_dir, header_session),
            Some(main_md.clone())
        );
        assert_eq!(
            session_markdown_write_path_for_key(&run_dir, header_session),
            main_md
        );
        assert_eq!(
            locate_session_markdown(&run_dir),
            Some(run_dir.join("run-20260524-161537-122998000.md"))
        );
    }

    #[test]
    fn parse_repo_example() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md");
        let blocks = parse_document(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].role(), Some("user"));
        assert_eq!(blocks[0].value_utf8().unwrap().trim(), "你好");
    }

    /// Parser behavior for `format: "persisting:1.0"` session markdown.
    mod parser_v1 {
        use super::*;

        fn block_header(role: &str, kind: &str) -> BlockHeader {
            let mut fields = BTreeMap::new();
            fields.insert("role".into(), serde_json::json!(role));
            fields.insert("kind".into(), serde_json::json!(kind));
            fields.insert("session_id".into(), serde_json::json!("test-session"));
            BlockHeader {
                type_name: "markdown".into(),
                length: 0,
                fields,
            }
        }

        fn canonical_doc(blocks: &[(BlockHeader, &[u8])]) -> String {
            let mut doc = baseline_preamble();
            for (header, body) in blocks {
                doc.push_str(&encode_block_with_header(header.clone(), body).unwrap());
            }
            doc
        }

        fn assert_block(
            block: &MarkdownBlock,
            expected_body: &[u8],
            expected_role: &str,
            expected_kind: &str,
        ) {
            assert_eq!(block.body, expected_body);
            assert_eq!(block.header.length, expected_body.len());
            assert_eq!(block.type_name(), "markdown");
            assert_eq!(block.role(), Some(expected_role));
            assert_eq!(block.kind(), Some(expected_kind));
        }

        #[test]
        fn frontmatter_only_yields_no_blocks() {
            let blocks = parse_document(&baseline_preamble()).unwrap();
            assert!(blocks.is_empty());
        }

        #[test]
        fn skips_yaml_frontmatter_before_first_block() {
            let doc = canonical_doc(&[(block_header("user", "llm.request"), b"ping")]);
            assert!(doc.starts_with("---\n"));
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks.len(), 1);
            assert_block(&blocks[0], b"ping", "user", "llm.request");
        }

        #[test]
        fn single_block_roundtrip_metadata() {
            let mut header = block_header("assistant", "llm.response");
            header.fields.insert("turn".into(), serde_json::json!(2));
            header
                .fields
                .insert("status".into(), serde_json::json!(200));
            header
                .fields
                .insert("model".into(), serde_json::json!("deepseek-chat"));

            let body = b"reply text";
            let doc = canonical_doc(&[(header.clone(), body)]);
            let blocks = parse_document(&doc).unwrap();

            assert_eq!(blocks.len(), 1);
            assert_block(&blocks[0], body, "assistant", "llm.response");
            assert_eq!(blocks[0].turn(), Some(2));
            assert_eq!(
                blocks[0]
                    .header
                    .fields
                    .get("status")
                    .and_then(|v| v.as_i64()),
                Some(200)
            );
            assert_eq!(
                blocks[0]
                    .header
                    .fields
                    .get("model")
                    .and_then(|v| v.as_str()),
                Some("deepseek-chat")
            );
        }

        #[test]
        fn multi_block_preserves_order_roles_and_bodies() {
            let doc = canonical_doc(&[
                (
                    block_header("user", "llm.request"),
                    "turn-1 user".as_bytes(),
                ),
                (
                    block_header("assistant", "llm.response"),
                    "turn-1 assistant".as_bytes(),
                ),
                (block_header("note", "note"), b"session metadata note"),
            ]);
            let blocks = parse_document(&doc).unwrap();

            assert_eq!(blocks.len(), 3);
            assert_block(&blocks[0], b"turn-1 user", "user", "llm.request");
            assert_block(&blocks[1], b"turn-1 assistant", "assistant", "llm.response");
            assert_block(&blocks[2], b"session metadata note", "note", "note");
        }

        #[test]
        fn blocks_separated_by_blank_line_after_body() {
            let encoded = encode_block_with_header(block_header("user", "llm.request"), b"a")
                .unwrap()
                + &encode_block_with_header(block_header("assistant", "llm.response"), b"b")
                    .unwrap();
            let doc = format!("{}{encoded}", baseline_preamble());
            assert!(doc.contains("a\n\n<!-- persisting:block:assistant"));
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].value_utf8().unwrap(), "a");
            assert_eq!(blocks[1].value_utf8().unwrap(), "b");
        }

        #[test]
        fn body_length_is_byte_exact_not_line_based() {
            // Body contains blank lines; parser must use `length`, not the next block marker.
            let inner = b"line one\n\nline two\n```\n fenced\n```";
            let doc = canonical_doc(&[(block_header("user", "llm.request"), inner)]);
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].body, inner);
            assert_eq!(blocks[0].header.length, inner.len());
        }

        #[test]
        fn unicode_body_uses_utf8_byte_length() {
            let body = "你好，世界！".as_bytes();
            let doc = canonical_doc(&[(block_header("user", "llm.request"), body)]);
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].body, body);
            assert_eq!(blocks[0].header.length, body.len());
            assert_eq!(blocks[0].value_utf8().unwrap(), "你好，世界！");
        }

        #[test]
        fn encode_then_parse_is_consistent_for_many_bodies() {
            let cases: &[(&str, &str, &[u8])] = &[
                ("user", "llm.request", b"plain"),
                ("assistant", "llm.response", b"multi\nline\nbody"),
                ("note", "note", b"---\n# not a preamble\n"),
                ("user", "llm.request", "emoji 🚀".as_bytes()),
            ];

            let pairs: Vec<_> = cases
                .iter()
                .map(|(role, kind, body)| (block_header(role, kind), *body))
                .collect();
            let doc = canonical_doc(&pairs);
            let parsed = parse_document(&doc).unwrap();

            assert_eq!(parsed.len(), cases.len());
            for (block, (_, _, expected_body)) in parsed.iter().zip(cases.iter()) {
                assert_eq!(block.body.as_slice(), *expected_body);
                assert_eq!(block.header.length, expected_body.len());
            }
        }

        #[test]
        fn double_parse_is_idempotent() {
            let doc = canonical_doc(&[
                (block_header("user", "llm.request"), b"q"),
                (block_header("assistant", "llm.response"), b"a"),
            ]);
            let first = parse_document(&doc).unwrap();
            let second = parse_document(&doc).unwrap();
            assert_eq!(first.len(), second.len());
            for (a, b) in first.iter().zip(second.iter()) {
                assert_eq!(a.body, b.body);
                assert_eq!(a.header.length, b.header.length);
                assert_eq!(a.role(), b.role());
                assert_eq!(a.kind(), b.kind());
            }
        }

        #[test]
        fn speaker_prefix_matches_role_field() {
            let doc = canonical_doc(&[(block_header("assistant", "llm.response"), b"ok")]);
            assert!(doc.contains("<!-- persisting:block:assistant "));
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks[0].role(), Some("assistant"));
        }

        #[test]
        fn note_role_defaults_speaker_to_note() {
            let mut header = block_header("note", "note");
            header.fields.remove("role");
            let doc = canonical_doc(&[(header, b"meta")]);
            assert!(doc.contains("<!-- persisting:block:note "));
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks[0].role(), None);
            assert_eq!(blocks[0].body, b"meta");
        }

        #[test]
        fn truncated_body_returns_error() {
            let doc = format!(
                "{}<!-- persisting:block:user {{\"type\":\"markdown\",\"length\":99,\"role\":\"user\",\"kind\":\"llm.request\",\"session_id\":\"x\"}} -->\n\nshort\n\n",
                baseline_preamble(),
            );
            let err = parse_document(&doc).unwrap_err();
            assert!(
                err.to_string().contains("past EOF"),
                "unexpected error: {err:#}"
            );
        }

        #[test]
        fn unclosed_frontmatter_returns_error() {
            let doc = "---\nformat: \"persisting:1.0\"\n";
            let err = parse_document(doc).unwrap_err();
            assert!(
                err.to_string().contains("unclosed YAML frontmatter"),
                "unexpected error: {err:#}"
            );
        }

        #[test]
        fn missing_block_marker_after_frontmatter_returns_error() {
            let doc = format!("{}not a block\n", baseline_preamble());
            let err = parse_document(&doc).unwrap_err();
            assert!(
                err.to_string().contains("expected `<!-- persisting:block"),
                "unexpected error: {err:#}"
            );
        }

        #[test]
        fn append_then_read_matches_canonical_layout() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(SESSION_MARKDOWN_FILENAME);

            let rec1 = llm_request_record(
                Some("sess-1".into()),
                Some("agent-1".into()),
                "deepseek-chat",
                "/v1/chat/completions",
                &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
            );
            let rec2 = llm_request_record(
                Some("sess-1".into()),
                Some("agent-1".into()),
                "deepseek-chat",
                "/v1/chat/completions",
                &serde_json::json!({"messages":[{"role":"user","content":"again"}]}),
            );
            let line1 = record_to_engine_line(&rec1).unwrap();
            let line2 = record_to_engine_line(&rec2).unwrap();
            append_engine_lines_to_markdown(&path, &[line1.as_str(), line2.as_str()]).unwrap();

            let on_disk = std::fs::read_to_string(&path).unwrap();
            assert!(on_disk.starts_with("---\n"));
            assert!(on_disk.contains("persisting:1.0"));
            assert!(on_disk.contains("message body"));
            assert!(on_disk.contains("persisting:block:{speaker}"));

            let blocks = parse_document(&on_disk).unwrap();
            assert_eq!(blocks.len(), 2);
            assert_eq!(blocks[0].role(), Some("user"));
            assert_eq!(blocks[1].role(), Some("user"));
            assert_eq!(blocks[0].header.length, blocks[0].body.len());
            assert_eq!(blocks[1].header.length, blocks[1].body.len());
        }

        #[test]
        fn replay_json_lines_aligns_with_parsed_blocks() {
            let doc = canonical_doc(&[
                (block_header("user", "llm.request"), b"question"),
                (block_header("assistant", "llm.response"), b"answer"),
            ]);
            let blocks = parse_document(&doc).unwrap();
            let lines = replay_json_lines(&blocks, 0, None).unwrap();
            assert_eq!(lines.len(), 2);
            for (line, block) in lines.iter().zip(blocks.iter()) {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                assert_eq!(
                    v.get("content").and_then(|c| c.as_str()),
                    Some(block.value_utf8().unwrap())
                );
                assert_eq!(v.get("role").and_then(|r| r.as_str()), block.role());
            }
        }

        #[test]
        fn preamble_includes_session_client_meta() {
            use crate::session_client::{write_session_client_meta, SessionClientMeta};

            let dir = tempfile::tempdir().unwrap();
            let session_dir = dir.path().join("demo-agent").join("sess-1");
            std::fs::create_dir_all(&session_dir).unwrap();
            write_session_client_meta(
                &session_dir.join(crate::session_client::SESSION_CLIENT_META_FILENAME),
                &SessionClientMeta {
                    peer: "127.0.0.1:54321".into(),
                    peer_port: 54321,
                    pid: 999,
                    command: "claude --model deepseek".into(),
                },
            )
            .unwrap();

            let md_path = session_dir.join(SESSION_MARKDOWN_FILENAME);
            let rec = llm_request_record(
                Some("sess-1".into()),
                Some("demo-agent".into()),
                "m",
                "/v1/chat/completions",
                &serde_json::json!({"messages":[{"role":"user","content":"hi"}]}),
            );
            let line = record_to_engine_line(&rec).unwrap();
            append_engine_lines_to_markdown(&md_path, &[line.as_str()]).unwrap();

            let text = std::fs::read_to_string(&md_path).unwrap();
            assert!(text.contains("client:"));
            assert!(text.contains("peer_port: 54321"));
            assert!(text.contains("claude --model deepseek"));

            let blocks = parse_document(&text).unwrap();
            assert_eq!(blocks.len(), 1);
        }

        #[test]
        fn parse_document_with_client_frontmatter() {
            let preamble =
                format_document_preamble(Some(&crate::session_client::SessionClientMeta {
                    peer: "127.0.0.1:40000".into(),
                    peer_port: 40000,
                    pid: 42,
                    command: "python3 agent.py".into(),
                }))
                .unwrap();
            let doc = format!(
                "{preamble}{}",
                encode_block_with_header(block_header("user", "llm.request"), b"hi").unwrap()
            );
            let blocks = parse_document(&doc).unwrap();
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].value_utf8().unwrap(), "hi");
        }
    }
}

#[cfg(test)]
mod upsert_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn block_with_call(call_id: &str, role: &str, body: &[u8]) -> (BlockHeader, Vec<u8>) {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), serde_json::json!(role));
        fields.insert("kind".into(), serde_json::json!("llm.response.stream"));
        fields.insert("call_id".into(), serde_json::json!(call_id));
        (
            BlockHeader {
                type_name: "markdown".into(),
                length: 0,
                fields,
            },
            body.to_vec(),
        )
    }

    #[test]
    fn upsert_rewrite_does_not_accumulate_trailing_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        for i in 0..50 {
            let body = format!("draft-{i}");
            upsert_block_by_call_id(
                &path,
                "call-1",
                block_with_call("call-1", "assistant", body.as_bytes()),
            )
            .unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("\n\n\n"),
            "upsert must not accumulate extra blank lines between blocks"
        );
        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].value_utf8().unwrap(), "draft-49");
    }

    #[test]
    fn upsert_replaces_block_body_by_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", b"draft"),
        )
        .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", b"final text"),
        )
        .unwrap();
        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].value_utf8().unwrap(), "final text");
    }

    #[test]
    fn upsert_appends_when_call_id_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", b"one"),
        )
        .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-2",
            block_with_call("call-2", "assistant", b"two"),
        )
        .unwrap();
        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].value_utf8().unwrap(), "one");
        assert_eq!(blocks[1].value_utf8().unwrap(), "two");
    }

    #[test]
    fn same_call_id_user_and_assistant_blocks_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(&path, "call-1", block_with_call("call-1", "user", b"hello"))
            .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", b"draft"),
        )
        .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", b"final answer"),
        )
        .unwrap();
        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].role(), Some("user"));
        assert_eq!(blocks[0].value_utf8().unwrap(), "hello");
        assert_eq!(blocks[1].role(), Some("assistant"));
        assert_eq!(blocks[1].value_utf8().unwrap(), "final answer");
    }

    #[test]
    fn upsert_assistant_rewrite_preserves_later_user_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        // call-A: user then streaming assistant draft (in-flight overlap setup)
        upsert_block_by_call_id(&path, "call-a", block_with_call("call-a", "user", b"req-a"))
            .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-a",
            block_with_call("call-a", "assistant", b"draft-a"),
        )
        .unwrap();
        // call-B request arrives before call-A response completes
        upsert_block_by_call_id(&path, "call-b", block_with_call("call-b", "user", b"req-b"))
            .unwrap();
        // call-A response finalizes — must not truncate call-B user
        upsert_block_by_call_id(
            &path,
            "call-a",
            block_with_call("call-a", "assistant", b"final-a"),
        )
        .unwrap();
        let blocks = read_blocks_from_file(&path).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].value_utf8().unwrap(), "req-a");
        assert_eq!(blocks[1].value_utf8().unwrap(), "final-a");
        assert_eq!(blocks[2].value_utf8().unwrap(), "req-b");
    }
}
