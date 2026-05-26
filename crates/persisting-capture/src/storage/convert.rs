//! Bidirectional conversion between Lance raw event log and TLV Markdown.
//!
//! - **Lance → Markdown** (`materialize`): full scan, overwrite document.
//! - **Lance → Markdown** (`stream`): incremental append per batch (capture `-f md`).
//! - **Markdown → Lance** (`compact`): reconstructs [`CaptureRecord`] rows.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::json;

use super::dialogue::{block_to_capture_record, capture_records_to_blocks};
use super::markdown::{
    format_document_preamble, locate_session_markdown_for_key, parse_document, BlockHeader,
    MarkdownBlock,
};
use super::record::{record_to_engine_line, CaptureRecord};
use super::session_client::{resolve_client_meta_for_run_dir, SessionClientMeta};

/// Outcome of Lance → Markdown materialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializeStats {
    pub source_events: usize,
    pub markdown_blocks: usize,
    pub skipped_events: usize,
}

/// Outcome of streaming Lance → Markdown (incremental block append).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMaterializeStats {
    pub events_seen: usize,
    pub blocks_appended: usize,
    pub skipped_events: usize,
}

/// Outcome of Markdown → Lance compaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub source_blocks: usize,
    pub lance_rows: usize,
}

/// Convert raw events to TLV blocks (applies dialogue filter; may skip rows).
pub fn capture_records_to_markdown_blocks(
    records: &[CaptureRecord],
) -> Result<(Vec<(BlockHeader, Vec<u8>)>, MaterializeStats)> {
    let blocks = capture_records_to_blocks(records)?;
    let skipped = records.len() - blocks.len();
    let markdown_blocks = blocks.len();
    Ok((
        blocks,
        MaterializeStats {
            source_events: records.len(),
            markdown_blocks,
            skipped_events: skipped,
        },
    ))
}

fn resolve_client_for_markdown(path: &Path) -> Option<SessionClientMeta> {
    path.parent().and_then(|run_dir| {
        run_dir
            .parent()
            .and_then(|agent_dir| agent_dir.parent())
            .and_then(|storage| resolve_client_meta_for_run_dir(storage, run_dir))
    })
}

/// Incrementally append dialogue blocks from engine lines (streaming materialize).
pub fn stream_engine_lines_to_markdown(
    run_dir: &Path,
    session_key: &str,
    engine_lines: &[impl AsRef<str>],
) -> Result<StreamMaterializeStats> {
    if engine_lines.is_empty() {
        return Ok(StreamMaterializeStats {
            events_seen: 0,
            blocks_appended: 0,
            skipped_events: 0,
        });
    }
    let md_path = materialize_markdown_path(run_dir, session_key);
    let records: Vec<CaptureRecord> = engine_lines
        .iter()
        .map(|line| crate::record::engine_line_to_record(line.as_ref()))
        .collect::<Result<_, _>>()?;
    let blocks = super::dialogue::capture_records_to_blocks(&records)?;
    let skipped = records.len() - blocks.len();
    write_markdown_document(&md_path, &blocks)?;
    let blocks_appended = blocks.len();
    Ok(StreamMaterializeStats {
        events_seen: engine_lines.len(),
        blocks_appended,
        skipped_events: skipped,
    })
}

/// Write a full TLV Markdown document from raw events (overwrites `path`).
pub fn materialize_records_to_markdown(
    path: &Path,
    records: &[CaptureRecord],
) -> Result<MaterializeStats> {
    let (blocks, stats) = capture_records_to_markdown_blocks(records)?;
    write_markdown_document(path, &blocks)?;
    Ok(stats)
}

/// Write blocks to a markdown file, replacing any existing content.
pub fn write_markdown_document(path: &Path, blocks: &[(BlockHeader, Vec<u8>)]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let client = resolve_client_for_markdown(path);
    let mut out = format_document_preamble(client.as_ref())?;
    for (header, body) in blocks {
        out.push_str(&crate::markdown_trajectory::encode_block_with_header(
            header.clone(),
            body,
        )?);
    }
    std::fs::write(path, out).with_context(|| format!("write markdown {}", path.display()))?;
    Ok(())
}

fn enrich_record_from_block(mut rec: CaptureRecord, block: &MarkdownBlock) -> CaptureRecord {
    rec.payload["_tlv"] = json!({
        "role": block.role(),
        "block_fields": block.header.fields,
    });
    rec
}

/// Parse a TLV Markdown document into capture records (for Lance compaction).
pub fn markdown_document_to_capture_records(doc: &str) -> Result<Vec<CaptureRecord>> {
    parse_document(doc)?
        .iter()
        .enumerate()
        .map(|(i, block)| {
            block_to_capture_record(block)
                .map(|rec| enrich_record_from_block(rec, block))
                .with_context(|| format!("block[{i}]"))
        })
        .collect::<Result<Vec<_>>>()
}

/// Markdown document → engine RON lines (one per block).
pub fn markdown_document_to_engine_lines(doc: &str) -> Result<String> {
    markdown_document_to_capture_records(doc)?
        .iter()
        .enumerate()
        .map(|(i, rec)| record_to_engine_line(rec).with_context(|| format!("record[{i}]")))
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join("\n"))
}

/// Resolve markdown path for materialization under a session run directory.
pub fn materialize_markdown_path(run_dir: &Path, session_key: &str) -> std::path::PathBuf {
    locate_session_markdown_for_key(run_dir, session_key).unwrap_or_else(|| {
        crate::markdown_trajectory::session_markdown_path_for_key(run_dir, session_key)
    })
}

pub fn compact_stats_note(
    stats: &CompactStats,
    md_path: &Path,
    lance_uri: &str,
    overwrite: bool,
) -> String {
    format!(
        "Compacted Markdown→Lance: {} block(s) → {} row(s) ({}) at {} → {}",
        stats.source_blocks,
        stats.lance_rows,
        if overwrite { "overwrite" } else { "append" },
        md_path.display(),
        lance_uri
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture_call::CaptureCall;
    use crate::lifecycle::{session_started_record, CaptureMode};
    use crate::record::record_to_engine_line;
    use crate::sink::{llm_request_record, llm_response_record};

    fn test_call() -> CaptureCall {
        CaptureCall {
            call_id: "call-1".into(),
            trace_id: "trace-1".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn roundtrip_markdown_compact_preserves_dialogue() {
        let call = test_call();
        let req = llm_request_record(
            Some("s".into()),
            None,
            "deepseek-chat",
            "/v1/chat/completions",
            &json!({"messages":[{"role":"user","content":"hi"}]}),
        );
        let resp = llm_response_record(
            Some("s".into()),
            None,
            200,
            &json!({"choices":[{"message":{"role":"assistant","content":"hello"}}]}),
            false,
            &call,
        );
        let (blocks, _) = capture_records_to_markdown_blocks(&[req, resp]).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.md");
        write_markdown_document(&path, &blocks).unwrap();
        let doc = std::fs::read_to_string(&path).unwrap();

        let compacted = markdown_document_to_capture_records(&doc).unwrap();
        assert_eq!(compacted.len(), 2);
        assert_eq!(compacted[0].kind, "llm.request");
        assert_eq!(compacted[1].kind, "llm.response");
        assert!(compacted[0].payload.get("_tlv").is_some());
    }

    #[test]
    fn stream_append_skips_lifecycle_incrementally() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("agent").join("sess");
        std::fs::create_dir_all(&run_dir).unwrap();

        let req = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1/count_tokens",
            &json!({"messages":[{"role":"user","content":"x"}]}),
        );
        let started = session_started_record(
            Some("s".into()),
            Some("a".into()),
            CaptureMode::Run,
            None,
            None,
        );
        let visible = llm_request_record(
            Some("s".into()),
            None,
            "m",
            "/v1/chat",
            &json!({"messages":[{"role":"user","content":"hello"}]}),
        );
        let lines = [req, started, visible]
            .iter()
            .map(record_to_engine_line)
            .collect::<Result<Vec<_>>>()
            .unwrap();

        let s1 = stream_engine_lines_to_markdown(&run_dir, "sess", &lines).unwrap();
        assert_eq!(s1.events_seen, 3);
        assert_eq!(s1.blocks_appended, 1);
        assert_eq!(s1.skipped_events, 2);

        let md_path = materialize_markdown_path(&run_dir, "sess");
        let text = std::fs::read_to_string(&md_path).unwrap();
        assert!(text.contains("hello"));
        assert!(!text.contains("session.started"));
    }

    #[test]
    fn stream_subagent_lines_use_isolated_md_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("deepseek-proxy").join("run-001");
        std::fs::create_dir_all(&run_dir).unwrap();
        let main_md = run_dir.join("run-001.md");
        std::fs::write(&main_md, "main-only\n").unwrap();

        let subagent_key = "agent-ad67e572475568b5a";
        let visible = llm_request_record(
            Some("37343ad1-ed7d-49dc-b080-9c4afd9873c2".into()),
            Some(subagent_key.strip_prefix("agent-").unwrap().into()),
            "m",
            "/v1/messages",
            &json!({"messages":[{"role":"user","content":"subagent task"}]}),
        );
        let line = record_to_engine_line(&visible).unwrap();
        let stats =
            stream_engine_lines_to_markdown(&run_dir, subagent_key, std::slice::from_ref(&line))
                .unwrap();
        assert_eq!(stats.blocks_appended, 1);

        let sub_md = materialize_markdown_path(&run_dir, subagent_key);
        assert_eq!(sub_md, run_dir.join("agent-ad67e572475568b5a.md"));
        let sub_text = std::fs::read_to_string(&sub_md).unwrap();
        assert!(sub_text.contains("subagent task"));

        let main_text = std::fs::read_to_string(&main_md).unwrap();
        assert_eq!(main_text.trim(), "main-only");
    }

    #[test]
    fn stream_main_session_does_not_append_to_subagent_md() {
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir
            .path()
            .join("deepseek-proxy")
            .join("run-20260524-161537-122998000");
        std::fs::create_dir_all(&run_dir).unwrap();
        let main_md = run_dir.join("run-20260524-161537-122998000.md");
        std::fs::write(&main_md, "main-only\n").unwrap();
        let sub_md = run_dir.join("agent-a2560e716f0b8b526.md");
        std::fs::write(&sub_md, "sub-only\n").unwrap();

        let header_session = "fb47835b-e10d-4b29-abc3-68f4594ebce3";
        let visible = llm_request_record(
            Some(header_session.into()),
            None,
            "deepseek-v4-pro",
            "/v1/messages",
            &json!({"messages":[{"role":"user","content":"main turn"}]}),
        );
        let line = record_to_engine_line(&visible).unwrap();
        let stats =
            stream_engine_lines_to_markdown(&run_dir, header_session, std::slice::from_ref(&line))
                .unwrap();
        assert_eq!(stats.blocks_appended, 1);

        let main_text = std::fs::read_to_string(&main_md).unwrap();
        assert!(main_text.contains("main turn"));
        let sub_text = std::fs::read_to_string(&sub_md).unwrap();
        assert_eq!(sub_text.trim(), "sub-only");
    }

    #[test]
    fn materialize_skips_replayed_turns_in_batch() {
        use crate::config::CaptureLevel;
        use crate::sink::{llm_request_summary_record, llm_response_record_with_content};

        let call = test_call();
        let mut req1 = llm_request_summary_record(
            Some("s".into()),
            None,
            "m",
            "/v1/messages",
            1,
            "messages",
            "anthropic",
            Some("hi".into()),
            None,
            &call,
            CaptureLevel::Dialogue,
            None,
        );
        req1.payload["user_message_count"] = json!(1);
        let resp1 = llm_response_record_with_content(
            Some("s".into()),
            None,
            200,
            &json!({"status": 200}),
            true,
            Some("Hello".into()),
            &call,
            CaptureLevel::Dialogue,
        );
        let mut replay = req1.clone();
        replay.call_id = Some("call-replay".into());
        let mut req2 = req1.clone();
        req2.call_id = Some("call-2".into());
        req2.payload["user_message_count"] = json!(2);
        req2.payload["user_content"] = json!("你好");
        let mut replay_call = call;
        replay_call.call_id = "call-2".into();
        let resp2 = llm_response_record_with_content(
            Some("s".into()),
            None,
            200,
            &json!({"status": 200}),
            true,
            Some("你好！".into()),
            &replay_call,
            CaptureLevel::Dialogue,
        );

        let (blocks, stats) =
            capture_records_to_markdown_blocks(&[req1, resp1, replay, req2, resp2]).unwrap();
        assert_eq!(stats.source_events, 5);
        assert_eq!(stats.markdown_blocks, 4);
        assert_eq!(stats.skipped_events, 1);
        assert_eq!(blocks.len(), 4);
    }
}
