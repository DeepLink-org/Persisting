//! Append / upsert markdown blocks on disk.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::dialogue::{block_to_replay_json, capture_records_to_blocks};
use crate::storage::session_client::resolve_client_meta_for_run_dir;

use super::parse::{
    block_speaker, parse_one_block, skip_preamble, validate_speaker, validate_type_name,
};
use super::preamble::format_document_preamble;
use super::types::{BlockHeader, MarkdownBlock, BLOCK_MARKER};

pub fn append_engine_lines_to_markdown(
    path: &Path,
    engine_lines: &[impl AsRef<str>],
) -> Result<usize> {
    if engine_lines.is_empty() {
        return Ok(0);
    }
    let blocks = capture_records_to_blocks(
        &engine_lines
            .iter()
            .map(|line| crate::record::engine_line_to_record(line.as_ref()))
            .collect::<Result<Vec<_>, _>>()?,
    )?;
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

pub(crate) fn find_block_by_call_id_and_role(
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
pub(crate) fn rewrite_block_range(
    path: &Path,
    start: usize,
    end: usize,
    new_block: &[u8],
) -> Result<()> {
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
