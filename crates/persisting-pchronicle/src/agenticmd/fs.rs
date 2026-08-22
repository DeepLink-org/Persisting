//! Filesystem helpers for the non-authoritative AgenticMD debug view.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::codec::{
    encode_agenticmd_block, encode_agenticmd_preamble, parse_agenticmd_blocks_with_spans,
    parse_agenticmd_document, MarkdownBlock, MarkdownBlockSpan, MarkdownHeader,
    AGENTICMD_BLOCK_LAYOUT, AGENTICMD_FRONTMATTER_FORMAT,
};
use super::convert::{encode_storyline_preamble, storyline_turn_block};
use super::validate::{
    block_speaker, validate_agenticmd_block, validate_speaker, validate_type_name,
};
use crate::{InputResult, StorylineDocument, StorylineTurn};

/// Diagnostic index of one AgenticMD document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgenticmdFileIndex {
    pub block_count: usize,
    pub call_ids: BTreeSet<String>,
    pub structural_issues: Vec<String>,
}

#[derive(Serialize)]
struct DefaultPreamble {
    format: &'static str,
    block: &'static str,
}

fn default_document_preamble() -> Result<String> {
    encode_agenticmd_preamble(&DefaultPreamble {
        format: AGENTICMD_FRONTMATTER_FORMAT,
        block: AGENTICMD_BLOCK_LAYOUT,
    })
    .map_err(|e| anyhow::anyhow!("agenticmd preamble: {e}"))
}

/// Encode one generated block after minimal comment-safety validation.
pub fn encode_agenticmd_block_validated(block: &MarkdownBlock) -> Result<String> {
    validate_type_name(&block.header.type_name)?;
    validate_speaker(block_speaker(&block.header))?;
    let mut block = block.clone();
    block.header.length = block.body.len();
    encode_agenticmd_block(&block).map_err(|e| anyhow::anyhow!("agenticmd encode: {e}"))
}

/// Tolerant parse plus minimal safety checks for fields used in generated comments.
pub fn parse_agenticmd_document_validated(input: &str) -> InputResult<Vec<MarkdownBlock>> {
    parse_agenticmd_document(input)?
        .blocks
        .into_iter()
        .enumerate()
        .map(|(i, block)| {
            validate_agenticmd_block(&block).map_err(|error| error.at(format!("blocks[{i}]")))?;
            Ok(block)
        })
        .collect::<InputResult<Vec<_>>>()
}

/// Tolerant parse with absolute byte spans for live-view upsert ranges.
pub fn parse_agenticmd_spans_validated(
    input: &str,
) -> InputResult<Vec<(MarkdownBlock, usize, usize)>> {
    let spans = parse_agenticmd_blocks_with_spans(input)?;
    spans
        .into_iter()
        .enumerate()
        .map(|(i, span)| {
            let MarkdownBlockSpan { block, start, end } = span;
            validate_agenticmd_block(&block).map_err(|error| error.at(format!("blocks[{i}]")))?;
            Ok((block, start, end))
        })
        .collect::<InputResult<Vec<_>>>()
}

/// Append blocks to a session markdown file.
///
/// When the file is empty / new, writes `empty_file_preamble` (or a default
/// `persisting` frontmatter when `None`).
pub fn append_agenticmd_blocks(
    path: &Path,
    blocks: &[MarkdownBlock],
    empty_file_preamble: Option<&str>,
) -> Result<usize> {
    if blocks.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    if file.metadata()?.len() == 0 {
        let preamble = match empty_file_preamble {
            Some(p) => p.to_string(),
            None => default_document_preamble()?,
        };
        file.write_all(preamble.as_bytes())?;
    }
    for block in blocks {
        file.write_all(encode_agenticmd_block_validated(block)?.as_bytes())?;
    }
    file.sync_all().ok();
    Ok(blocks.len())
}

/// Replace a complete AgenticMD document with an already-encoded preamble and blocks.
pub fn write_agenticmd_document(
    path: &Path,
    preamble: &str,
    blocks: &[MarkdownBlock],
) -> Result<()> {
    let mut output = preamble.to_string();
    for block in blocks {
        output.push_str(&encode_agenticmd_block_validated(block)?);
    }
    write_atomic(path, output.as_bytes())
}

/// Atomically replace an AgenticMD view from its authoritative Storyline model.
pub fn write_agenticmd_storyline(path: &Path, story: &StorylineDocument) -> Result<()> {
    let encoded = super::convert::encode_agenticmd(story)
        .map_err(|error| anyhow::anyhow!("agenticmd encode: {error}"))?;
    write_atomic(path, encoded.as_bytes())
}

/// Insert or replace one Storyline turn in a live AgenticMD view.
///
/// `edit_key` is a syntax-only locator used for streaming draft replacement. It
/// is never copied into the parsed [`StorylineTurn`] or its `extra` field.
pub fn upsert_agenticmd_turn(
    path: &Path,
    document_meta: &StorylineDocument,
    turn: &StorylineTurn,
    edit_key: &str,
) -> Result<bool> {
    if edit_key.trim().is_empty() {
        bail!("edit_key must not be empty for AgenticMD upsert");
    }
    let mut candidate = document_meta.clone();
    candidate.turns = vec![turn.clone()];
    candidate
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid Storyline turn: {error}"))?;

    let block = storyline_turn_block(turn, Some(edit_key))
        .map_err(|error| anyhow::anyhow!("agenticmd turn encode: {error}"))?;
    if !path.exists() {
        let preamble = encode_storyline_preamble(document_meta)
            .map_err(|error| anyhow::anyhow!("agenticmd preamble encode: {error}"))?;
        write_agenticmd_document(path, &preamble, std::slice::from_ref(&block))?;
        return Ok(false);
    }
    upsert_block_by_call_id(path, edit_key, block)
}

/// Replace only the YAML preamble while preserving every encoded block byte-for-byte.
pub fn rewrite_agenticmd_preamble(path: &Path, preamble: &str) -> Result<()> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let body_start = super::codec::agenticmd_body_byte_offset(&content)
        .map_err(|e| anyhow::anyhow!("agenticmd body offset: {e}"))?;
    let mut output = preamble.as_bytes().to_vec();
    output.extend_from_slice(&content.as_bytes()[body_start..]);
    write_atomic(path, &output)
}

/// Replace only the Storyline document metadata in an AgenticMD file.
///
/// Existing encoded turns remain byte-for-byte intact, including private live
/// edit locators used to replace streaming drafts.
pub fn rewrite_agenticmd_storyline_metadata(
    path: &Path,
    document_meta: &StorylineDocument,
) -> Result<()> {
    let preamble = encode_storyline_preamble(document_meta)
        .map_err(|error| anyhow::anyhow!("agenticmd preamble encode: {error}"))?;
    rewrite_agenticmd_preamble(path, &preamble)
}

/// List AgenticMD candidates directly below a run directory.
pub fn list_agenticmd_paths(run_dir: &Path) -> Result<Vec<PathBuf>> {
    if !run_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in
        std::fs::read_dir(run_dir).with_context(|| format!("read_dir {}", run_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("md") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Inspect block count, call IDs, and format-level structural issues.
pub fn index_agenticmd_path(path: &Path) -> Result<AgenticmdFileIndex> {
    let raw = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };
    let blocks = if raw.is_empty() {
        Vec::new()
    } else {
        parse_agenticmd_document_validated(&raw)?
    };
    let call_ids = blocks
        .iter()
        .filter_map(|block| {
            block
                .header
                .fields
                .get("call_id")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .collect();
    Ok(AgenticmdFileIndex {
        block_count: blocks.len(),
        call_ids,
        structural_issues: agenticmd_structural_issues(&raw),
    })
}

/// Detect structural anomalies independent of any capture producer.
pub fn agenticmd_structural_issues(document: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if document.contains("\n\n\n\n") {
        issues.push("excessive_blank_lines".to_string());
    }
    issues
}

/// Count blocks with a specific AgenticMD `role`.
pub fn count_agenticmd_role(path: &Path, role: &str) -> Result<u64> {
    Ok(read_agenticmd_blocks_from_file(path)?
        .iter()
        .filter(|block| block.role() == Some(role))
        .count() as u64)
}

/// Replace the block whose header `call_id` and presentation role match, or append when missing.
///
/// Returns `true` when an existing block was rewritten.
pub fn upsert_block_by_call_id(path: &Path, call_id: &str, block: MarkdownBlock) -> Result<bool> {
    if call_id.trim().is_empty() {
        bail!("call_id must not be empty for markdown upsert");
    }
    let role = block
        .role()
        .context("markdown upsert block missing role field")?
        .to_string();
    if !path.exists() {
        append_agenticmd_blocks(path, std::slice::from_ref(&block), None)?;
        return Ok(false);
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if let Some((start, end, _header)) = find_block_by_call_id_and_role(&bytes, call_id, &role)? {
        let encoded = encode_agenticmd_block_validated(&block)?;
        rewrite_block_range(path, start, end, encoded.as_bytes())?;
        Ok(true)
    } else {
        append_agenticmd_blocks(path, std::slice::from_ref(&block), None)?;
        Ok(false)
    }
}

pub fn find_block_by_call_id_and_role(
    bytes: &[u8],
    call_id: &str,
    role: &str,
) -> Result<Option<(usize, usize, MarkdownHeader)>> {
    let text = std::str::from_utf8(bytes).context("markdown upsert requires UTF-8 document")?;
    for (block, start, end) in parse_agenticmd_spans_validated(text)? {
        if block_matches_upsert_key(&block.header, call_id, role) {
            return Ok(Some((start, end, block.header)));
        }
    }
    Ok(None)
}

fn block_matches_upsert_key(header: &MarkdownHeader, call_id: &str, role: &str) -> bool {
    header.fields.get("call_id").and_then(|v| v.as_str()) == Some(call_id)
        && header_role(header) == role
}

fn header_role(header: &MarkdownHeader) -> &str {
    if let Some(role) = header.fields.get("role").and_then(|v| v.as_str()) {
        return role;
    }
    match header.fields.get("source").and_then(|v| v.as_str()) {
        Some("agent") => "assistant",
        Some("user") => "user",
        _ => "note",
    }
}

pub fn rewrite_block_range(path: &Path, start: usize, end: usize, new_block: &[u8]) -> Result<()> {
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
    write_atomic(path, &out)
}

/// Strict-parse all agenticmd blocks from a markdown file (empty if missing).
pub fn read_agenticmd_blocks_from_file(path: &Path) -> Result<Vec<MarkdownBlock>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(parse_agenticmd_document_validated(&text)?)
}

pub fn agenticmd_block_count(path: &Path) -> Result<usize> {
    Ok(read_agenticmd_blocks_from_file(path)?.len())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
    let mut file =
        std::fs::File::create(&temp).with_context(|| format!("create {}", temp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write {}", temp.display()))?;
    file.sync_all()
        .with_context(|| format!("sync {}", temp.display()))?;
    std::fs::rename(&temp, path)
        .with_context(|| format!("commit {} -> {}", temp.display(), path.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .with_context(|| format!("sync parent {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::codec::{
        encode_agenticmd_preamble, MarkdownHeader, AGENTICMD_BLOCK_LAYOUT,
        AGENTICMD_FRONTMATTER_FORMAT,
    };
    use super::*;
    use serde::Serialize;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::path::Path;

    #[derive(Serialize)]
    struct Preamble {
        format: &'static str,
        block: &'static str,
    }

    fn baseline_preamble() -> String {
        encode_agenticmd_preamble(&Preamble {
            format: AGENTICMD_FRONTMATTER_FORMAT,
            block: AGENTICMD_BLOCK_LAYOUT,
        })
        .unwrap()
    }

    fn block_with_call(call_id: &str, role: &str, body: &str) -> MarkdownBlock {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!("llm.response.stream"));
        fields.insert("call_id".into(), json!(call_id));
        MarkdownBlock {
            header: MarkdownHeader {
                type_name: "markdown".into(),
                length: body.len(),
                fields,
            },
            body: body.into(),
        }
    }

    fn block_header(role: &str, kind: &str) -> MarkdownHeader {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!(kind));
        fields.insert("session_id".into(), json!("test-session"));
        MarkdownHeader {
            type_name: "markdown".into(),
            length: 0,
            fields,
        }
    }

    fn encode_block(header: MarkdownHeader, body: &str) -> String {
        encode_agenticmd_block_validated(&MarkdownBlock {
            header,
            body: body.into(),
        })
        .unwrap()
    }

    fn canonical_doc(blocks: &[(MarkdownHeader, &str)]) -> String {
        let mut doc = baseline_preamble();
        for (header, body) in blocks {
            doc.push_str(&encode_block(header.clone(), body));
        }
        doc
    }

    fn read_blocks(path: &Path) -> Vec<MarkdownBlock> {
        let text = std::fs::read_to_string(path).unwrap();
        parse_agenticmd_document_validated(&text).unwrap()
    }

    #[test]
    fn upsert_rewrite_does_not_accumulate_trailing_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        for i in 0..50 {
            upsert_block_by_call_id(
                &path,
                "call-1",
                block_with_call("call-1", "assistant", &format!("draft-{i}")),
            )
            .unwrap();
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("\n\n\n"));
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].body, "draft-49");
    }

    #[test]
    fn upsert_replaces_block_body_by_call_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", "draft"),
        )
        .unwrap();
        assert!(upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", "final")
        )
        .unwrap());
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].body, "final");
    }

    #[test]
    fn upsert_appends_when_call_id_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(&path, "call-1", block_with_call("call-1", "assistant", "a"))
            .unwrap();
        assert!(!upsert_block_by_call_id(
            &path,
            "call-2",
            block_with_call("call-2", "assistant", "b")
        )
        .unwrap());
        assert_eq!(read_blocks(&path).len(), 2);
    }

    #[test]
    fn same_call_id_user_and_assistant_blocks_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(&path, "call-1", block_with_call("call-1", "user", "hello"))
            .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", "draft"),
        )
        .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-1",
            block_with_call("call-1", "assistant", "final"),
        )
        .unwrap();
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].role(), Some("user"));
        assert_eq!(blocks[0].body, "hello");
        assert_eq!(blocks[1].role(), Some("assistant"));
        assert_eq!(blocks[1].body, "final");
    }

    #[test]
    fn upsert_assistant_rewrite_preserves_later_user_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.md");
        upsert_block_by_call_id(&path, "call-a", block_with_call("call-a", "user", "req-a"))
            .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-a",
            block_with_call("call-a", "assistant", "draft-a"),
        )
        .unwrap();
        upsert_block_by_call_id(&path, "call-b", block_with_call("call-b", "user", "req-b"))
            .unwrap();
        upsert_block_by_call_id(
            &path,
            "call-a",
            block_with_call("call-a", "assistant", "final-a"),
        )
        .unwrap();
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].body, "req-a");
        assert_eq!(blocks[1].body, "final-a");
        assert_eq!(blocks[2].body, "req-b");
    }

    #[test]
    fn body_may_contain_markdown_syntax() {
        let inner = "---\n```rust\nfn main() {}\n```\n";
        let doc = encode_block(block_header("note", "note"), inner);
        assert_eq!(
            parse_agenticmd_document_validated(&doc).unwrap()[0].body,
            inner
        );
    }

    #[test]
    fn validated_in_memory_parse_retains_leaf_issue_location() {
        let error = parse_agenticmd_document_validated("---\nformat: [\n---\n\n").unwrap_err();

        assert_eq!(error.kind(), crate::input::InputIssueKind::Invalid);
        assert_eq!(error.location(), Some("frontmatter"));
    }

    #[test]
    fn block_comment_separated_from_body_by_blank_line() {
        let encoded = encode_block(block_header("user", "llm.request"), "hi");
        assert!(encoded.contains("-->\n\nhi\n\n"));
    }

    #[test]
    fn block_marker_includes_speaker() {
        let encoded = encode_block(block_header("user", "llm.request"), "hey");
        assert!(encoded.starts_with("<!-- persisting:block:user "));
        assert_eq!(
            parse_agenticmd_document_validated(&encoded).unwrap().len(),
            1
        );
    }

    #[test]
    fn accepts_marker_without_speaker_but_rejects_unframed_prefix() {
        let marker_without_speaker = "<!-- persisting:block {\"type\":\"markdown\",\"length\":2,\"role\":\"assistant\"} -->\nok\n";
        let parsed = parse_agenticmd_document_validated(marker_without_speaker).unwrap();
        assert_eq!(parsed[0].body, "ok");
        assert_eq!(parsed[0].source(), Some("agent"));

        let hash_preamble = format!(
            "# old preamble\n{}",
            encode_block(block_header("note", "note"), "hi")
        );
        assert!(parse_agenticmd_document_validated(&hash_preamble).is_err());
    }

    #[test]
    fn validated_encode_matches_raw_encode_after_length_set() {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!("assistant"));
        fields.insert("kind".into(), json!("llm.response"));
        fields.insert("call_id".into(), json!("c1"));
        let body = "hello world";
        let via_validated = encode_agenticmd_block_validated(&MarkdownBlock {
            header: MarkdownHeader {
                type_name: "text".into(),
                length: 0,
                fields: fields.clone(),
            },
            body: body.into(),
        })
        .unwrap();
        let via_raw = super::super::codec::encode_agenticmd_block(&MarkdownBlock {
            header: MarkdownHeader {
                type_name: "text".into(),
                length: body.len(),
                fields,
            },
            body: body.into(),
        })
        .unwrap();
        assert_eq!(via_validated, via_raw);
    }

    #[test]
    fn spans_cover_full_encoded_block_for_upsert() {
        let encoded = encode_block(block_header("assistant", "llm.response"), "draft");
        let doc = format!("{}{encoded}", baseline_preamble());
        let spans = parse_agenticmd_spans_validated(&doc).unwrap();
        assert_eq!(spans.len(), 1);
        let (parsed, start, end) = &spans[0];
        assert_eq!(parsed.body, "draft");
        assert_eq!(&doc.as_bytes()[*start..*end], encoded.as_bytes());
    }

    #[test]
    fn frontmatter_only_yields_no_blocks() {
        assert!(parse_agenticmd_document_validated(&baseline_preamble())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn multi_block_preserves_order_roles_and_bodies() {
        let doc = canonical_doc(&[
            (block_header("user", "llm.request"), "turn-1 user"),
            (
                block_header("assistant", "llm.response"),
                "turn-1 assistant",
            ),
            (block_header("note", "note"), "session metadata note"),
        ]);
        let blocks = parse_agenticmd_document_validated(&doc).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].body, "turn-1 user");
        assert_eq!(blocks[1].body, "turn-1 assistant");
        assert_eq!(blocks[2].body, "session metadata note");
    }

    #[test]
    fn unicode_body_uses_utf8_byte_length() {
        let body = "你好，世界！";
        let doc = canonical_doc(&[(block_header("user", "llm.request"), body)]);
        let blocks = parse_agenticmd_document_validated(&doc).unwrap();
        assert_eq!(blocks[0].body, body);
        assert_eq!(blocks[0].header.length, body.len());
    }

    #[test]
    fn truncated_body_returns_error() {
        let doc = format!(
            "{}<!-- persisting:block:user {{\"type\":\"markdown\",\"length\":99,\"role\":\"user\",\"kind\":\"llm.request\",\"session_id\":\"x\"}} -->\n\nshort\n\n",
            baseline_preamble(),
        );
        let err = parse_agenticmd_document_validated(&doc).unwrap_err();
        assert!(err.to_string().contains("past EOF"), "{err:#}");
    }

    #[test]
    fn unclosed_frontmatter_returns_error() {
        let err = parse_agenticmd_document_validated("---\nformat: \"persisting\"\n").unwrap_err();
        assert!(
            err.to_string().contains("unclosed YAML frontmatter"),
            "{err:#}"
        );
    }

    #[test]
    fn plain_markdown_after_frontmatter_becomes_debug_block() {
        let doc = format!("{}not a block\n", baseline_preamble());
        let parsed = parse_agenticmd_document_validated(&doc).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].body, "not a block");
        assert_eq!(parsed[0].source(), Some("system"));
    }

    #[test]
    fn append_then_read_matches_canonical_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-test.md");
        append_agenticmd_blocks(
            &path,
            &[
                block_with_call("c1", "user", "hi"),
                block_with_call("c2", "user", "again"),
            ],
            None,
        )
        .unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("---\n"));
        assert!(on_disk.contains("persisting"));
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].body, "hi");
        assert_eq!(blocks[1].body, "again");
    }

    #[test]
    fn document_io_and_index_are_owned_together() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.md");
        let blocks = vec![
            block_with_call("c1", "user", "hi"),
            block_with_call("c1", "assistant", "hello"),
        ];
        write_agenticmd_document(&path, &baseline_preamble(), &blocks).unwrap();

        let index = index_agenticmd_path(&path).unwrap();
        assert_eq!(index.block_count, 2);
        assert_eq!(index.call_ids, BTreeSet::from(["c1".to_string()]));
        assert!(index.structural_issues.is_empty());
        assert_eq!(count_agenticmd_role(&path, "user").unwrap(), 1);
        assert_eq!(
            list_agenticmd_paths(dir.path()).unwrap(),
            vec![path.clone()]
        );

        #[derive(Serialize)]
        struct SummaryPreamble {
            format: &'static str,
            block: &'static str,
            turns: u64,
        }
        let replacement = encode_agenticmd_preamble(&SummaryPreamble {
            format: AGENTICMD_FRONTMATTER_FORMAT,
            block: AGENTICMD_BLOCK_LAYOUT,
            turns: 1,
        })
        .unwrap();
        rewrite_agenticmd_preamble(&path, &replacement).unwrap();
        let rewritten = std::fs::read_to_string(&path).unwrap();
        assert!(rewritten.contains("turns: 1"));
        assert_eq!(read_blocks(&path), blocks);
    }

    #[test]
    fn structural_scan_reports_excessive_blank_lines() {
        assert_eq!(
            agenticmd_structural_issues("a\n\n\n\nb"),
            vec!["excessive_blank_lines"]
        );
    }

    #[test]
    fn storyline_upsert_replaces_draft_without_exposing_edit_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.md");
        let story = crate::StorylineDocument::new("session-1", "agent-1");
        let mut turn = crate::StorylineTurn {
            id: 7,
            kind: Some("llm.response.stream".into()),
            timestamp: Some(
                crate::model::StorylineTimestamp::from_rfc3339("2026-01-01T00:00:00Z").unwrap(),
            ),
            source: "agent".into(),
            message: serde_json::json!("draft"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: Some("model-1".into()),
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: Some(12),
            extra: Some(serde_json::json!({"domain": "kept"})),
            env: None,
            prompt: None,
            finished_at: None,
        };

        assert!(!upsert_agenticmd_turn(&path, &story, &turn, "call-7").unwrap());
        turn.kind = Some("llm.response".into());
        turn.message = serde_json::json!("complete");
        turn.latency_ms = Some(42);
        assert!(upsert_agenticmd_turn(&path, &story, &turn, "call-7").unwrap());

        let parsed =
            super::super::convert::parse_agenticmd(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(parsed.turns, vec![turn]);
        assert_eq!(
            parsed.turns[0].extra,
            Some(serde_json::json!({"domain": "kept"}))
        );
        assert!(index_agenticmd_path(&path)
            .unwrap()
            .call_ids
            .contains("call-7"));
    }
}
