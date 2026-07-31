//! Filesystem store for agenticmd session markdown (append + call_id upsert).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::formats::agenticmd::{
    encode_agenticmd_block, encode_agenticmd_preamble, parse_agenticmd_blocks_with_spans,
    parse_agenticmd_document_with, AgenticmdBlock, AgenticmdBlockSpan, AgenticmdHeader,
    AgenticmdParseMode, AGENTICMD_BLOCK_LAYOUT, AGENTICMD_FRONTMATTER_FORMAT,
};
use crate::formats::agenticmd_validate::{
    block_speaker, validate_agenticmd_block, validate_speaker, validate_type_name,
};

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

/// Encode one block after speaker/type validation (sets `header.length`).
pub fn encode_agenticmd_block_validated(block: &AgenticmdBlock) -> Result<String> {
    validate_type_name(&block.header.type_name)?;
    validate_speaker(block_speaker(&block.header))?;
    let mut block = block.clone();
    block.header.length = block.body.len();
    encode_agenticmd_block(&block).map_err(|e| anyhow::anyhow!("agenticmd encode: {e}"))
}

/// Strict parse with speaker/type checks.
pub fn parse_agenticmd_document_validated(input: &str) -> Result<Vec<AgenticmdBlock>> {
    parse_agenticmd_document_with(input, AgenticmdParseMode::Strict)
        .map_err(|e| anyhow::anyhow!("agenticmd parse: {e}"))?
        .blocks
        .into_iter()
        .enumerate()
        .map(|(i, block)| {
            validate_agenticmd_block(&block).with_context(|| format!("agenticmd block[{i}]"))?;
            Ok(block)
        })
        .collect()
}

/// Strict parse with absolute byte spans (upsert rewrite ranges).
pub fn parse_agenticmd_spans_validated(input: &str) -> Result<Vec<(AgenticmdBlock, usize, usize)>> {
    let spans = parse_agenticmd_blocks_with_spans(input, AgenticmdParseMode::Strict)
        .map_err(|e| anyhow::anyhow!("agenticmd span parse: {e}"))?;
    spans
        .into_iter()
        .enumerate()
        .map(|(i, span)| {
            let AgenticmdBlockSpan { block, start, end } = span;
            validate_agenticmd_block(&block)
                .with_context(|| format!("agenticmd span block[{i}]"))?;
            Ok((block, start, end))
        })
        .collect()
}

/// Append blocks to a session markdown file.
///
/// When the file is empty / new, writes `empty_file_preamble` (or a default
/// `persisting:1.0` frontmatter when `None`).
pub fn append_agenticmd_blocks(
    path: &Path,
    blocks: &[AgenticmdBlock],
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

/// Replace the block whose header `call_id` and `role` match, or append when missing.
///
/// Returns `true` when an existing block was rewritten.
pub fn upsert_block_by_call_id(path: &Path, call_id: &str, block: AgenticmdBlock) -> Result<bool> {
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
) -> Result<Option<(usize, usize, AgenticmdHeader)>> {
    let text = std::str::from_utf8(bytes).context("markdown upsert requires UTF-8 document")?;
    for (block, start, end) in parse_agenticmd_spans_validated(text)? {
        if block_matches_upsert_key(&block.header, call_id, role) {
            return Ok(Some((start, end, block.header)));
        }
    }
    Ok(None)
}

fn block_matches_upsert_key(header: &AgenticmdHeader, call_id: &str, role: &str) -> bool {
    header.fields.get("call_id").and_then(|v| v.as_str()) == Some(call_id)
        && header.fields.get("role").and_then(|v| v.as_str()) == Some(role)
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
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Strict-parse all agenticmd blocks from a markdown file (empty if missing).
pub fn read_agenticmd_blocks_from_file(path: &Path) -> Result<Vec<AgenticmdBlock>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_agenticmd_document_validated(&text)
}

pub fn agenticmd_block_count(path: &Path) -> Result<usize> {
    Ok(read_agenticmd_blocks_from_file(path)?.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::agenticmd::{
        encode_agenticmd_preamble, AgenticmdHeader, AGENTICMD_BLOCK_LAYOUT,
        AGENTICMD_FRONTMATTER_FORMAT, BLOCK_MARKER,
    };
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

    fn block_with_call(call_id: &str, role: &str, body: &str) -> AgenticmdBlock {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!("llm.response.stream"));
        fields.insert("call_id".into(), json!(call_id));
        AgenticmdBlock {
            header: AgenticmdHeader {
                type_name: "markdown".into(),
                length: body.len(),
                fields,
            },
            body: body.into(),
        }
    }

    fn block_header(role: &str, kind: &str) -> AgenticmdHeader {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!(kind));
        fields.insert("session_id".into(), json!("test-session"));
        AgenticmdHeader {
            type_name: "markdown".into(),
            length: 0,
            fields,
        }
    }

    fn encode_block(header: AgenticmdHeader, body: &str) -> String {
        encode_agenticmd_block_validated(&AgenticmdBlock {
            header,
            body: body.into(),
        })
        .unwrap()
    }

    fn canonical_doc(blocks: &[(AgenticmdHeader, &str)]) -> String {
        let mut doc = baseline_preamble();
        for (header, body) in blocks {
            doc.push_str(&encode_block(header.clone(), body));
        }
        doc
    }

    fn read_blocks(path: &Path) -> Vec<AgenticmdBlock> {
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
    fn parse_legacy_block_marker_without_speaker() {
        let doc = format!(
            "{BLOCK_MARKER} {{\"type\":\"markdown\",\"length\":2,\"role\":\"assistant\"}} -->\nok\n"
        );
        let blocks = parse_agenticmd_document_validated(&doc).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].role(), Some("assistant"));
    }

    #[test]
    fn parse_legacy_hash_preamble() {
        let doc = format!(
            "# persisting trajectory\n# legacy header\n\n{}",
            encode_block(block_header("note", "note"), "hi")
        );
        let blocks = parse_agenticmd_document_validated(&doc).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].body, "hi");
    }

    #[test]
    fn parse_repo_example() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md");
        let blocks =
            parse_agenticmd_document_validated(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].role(), Some("user"));
        assert_eq!(blocks[0].body.trim(), "你好");
    }

    #[test]
    fn validated_encode_matches_raw_encode_after_length_set() {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!("assistant"));
        fields.insert("kind".into(), json!("llm.response"));
        fields.insert("call_id".into(), json!("c1"));
        let body = "hello world";
        let via_validated = encode_agenticmd_block_validated(&AgenticmdBlock {
            header: AgenticmdHeader {
                type_name: "text".into(),
                length: 0,
                fields: fields.clone(),
            },
            body: body.into(),
        })
        .unwrap();
        let via_raw = crate::formats::agenticmd::encode_agenticmd_block(&AgenticmdBlock {
            header: AgenticmdHeader {
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
        let err =
            parse_agenticmd_document_validated("---\nformat: \"persisting:1.0\"\n").unwrap_err();
        assert!(
            err.to_string().contains("unclosed YAML frontmatter"),
            "{err:#}"
        );
    }

    #[test]
    fn missing_block_marker_after_frontmatter_returns_error() {
        let doc = format!("{}not a block\n", baseline_preamble());
        let err = parse_agenticmd_document_validated(&doc).unwrap_err();
        assert!(
            err.to_string().contains("expected `<!-- persisting:block"),
            "{err:#}"
        );
    }

    #[test]
    fn append_then_read_matches_canonical_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0001.md");
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
        assert!(on_disk.contains("persisting:1.0"));
        let blocks = read_blocks(&path);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].body, "hi");
        assert_eq!(blocks[1].body, "again");
    }
}
