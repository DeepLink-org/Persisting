//! Capture-owned markdown IO: pipeline append, client-meta preamble, upsert seed.
//! Codec / layout / call_id rewrite coverage lives in `persisting-pchronicle`.

use std::collections::BTreeMap;

use persisting_capture::markdown_trajectory::{
    append_engine_lines_to_markdown, encode_agenticmd_block_validated, format_document_preamble,
    parse_document, read_blocks_from_file, replay_json_lines, upsert_block_by_call_id,
};
use persisting_capture::record::record_to_engine_line;
use persisting_capture::session_client::{
    write_session_client_meta, SessionClientMeta, SESSION_CLIENT_META_FILENAME,
};
use persisting_capture::sink::llm_request_record;
use persisting_pchronicle::{
    agenticmd_body_byte_offset, AgenticmdBlock, AgenticmdHeader, BLOCK_MARKER,
    SESSION_MARKDOWN_FILENAME,
};

fn block_with_call(call_id: &str, role: &str, body: &str) -> AgenticmdBlock {
    let mut fields = BTreeMap::new();
    fields.insert("role".into(), serde_json::json!(role));
    fields.insert("kind".into(), serde_json::json!("llm.response.stream"));
    fields.insert("call_id".into(), serde_json::json!(call_id));
    AgenticmdBlock {
        header: AgenticmdHeader {
            type_name: "markdown".into(),
            length: body.len(),
            fields,
        },
        body: body.into(),
    }
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

    let blocks = parse_document(&on_disk).unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].role(), Some("user"));
    assert_eq!(blocks[1].role(), Some("user"));
}

#[test]
fn replay_json_lines_aligns_with_parsed_blocks() {
    fn header(role: &str, kind: &str) -> AgenticmdHeader {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), serde_json::json!(role));
        fields.insert("kind".into(), serde_json::json!(kind));
        AgenticmdHeader {
            type_name: "markdown".into(),
            length: 0,
            fields,
        }
    }

    let blocks = vec![
        AgenticmdBlock {
            header: header("user", "llm.request"),
            body: "question".into(),
        },
        AgenticmdBlock {
            header: header("assistant", "llm.response"),
            body: "answer".into(),
        },
    ];
    let lines = replay_json_lines(&blocks, 0, None).unwrap();
    assert_eq!(lines.len(), 2);
    for (line, block) in lines.iter().zip(blocks.iter()) {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            v.get("content").and_then(|c| c.as_str()),
            Some(block.body.as_str())
        );
        assert_eq!(v.get("role").and_then(|r| r.as_str()), block.role());
    }
}

#[test]
fn preamble_includes_session_client_meta() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("demo-agent").join("sess-1");
    std::fs::create_dir_all(&session_dir).unwrap();
    write_session_client_meta(
        &session_dir.join(SESSION_CLIENT_META_FILENAME),
        &SessionClientMeta {
            peer: "127.0.0.1:54321".into(),
            peer_port: 54321,
            pid: 999,
            command: "claude --model deepseek".into(),
            machine_fp: None,
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
    let preamble = format_document_preamble(Some(&SessionClientMeta {
        peer: "127.0.0.1:40000".into(),
        peer_port: 40000,
        pid: 42,
        command: "python3 agent.py".into(),
        machine_fp: None,
    }))
    .unwrap();
    let mut fields = BTreeMap::new();
    fields.insert("role".into(), serde_json::json!("user"));
    fields.insert("kind".into(), serde_json::json!("llm.request"));
    let block = encode_agenticmd_block_validated(&AgenticmdBlock {
        header: AgenticmdHeader {
            type_name: "markdown".into(),
            length: 2,
            fields,
        },
        body: "hi".into(),
    })
    .unwrap();
    let doc = format!("{preamble}{block}");
    let blocks = parse_document(&doc).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].body, "hi");
}

#[test]
fn upsert_new_file_seeds_capture_preamble() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess.md");
    assert!(!upsert_block_by_call_id(
        &path,
        "call-1",
        block_with_call("call-1", "assistant", "hello"),
    )
    .unwrap());
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.starts_with("---\n"));
    assert!(raw.contains("persisting:1.0"));
    assert!(raw.contains("persisting:block:{speaker}"));
    let blocks = read_blocks_from_file(&path).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].body, "hello");
}

#[test]
fn preamble_roundtrips_through_body_offset() {
    let preamble = format_document_preamble(None).unwrap();
    assert!(preamble.starts_with("---\n"));
    assert!(preamble.contains("format: persisting:1.0"));
    assert!(preamble.contains("block:"));
    let start = agenticmd_body_byte_offset(&preamble).unwrap();
    assert!(
        preamble[start..].trim().is_empty(),
        "body after document preamble should be blank, got {:?}",
        &preamble[start..]
    );
}

#[test]
fn preamble_embeds_nested_client() {
    let preamble = format_document_preamble(Some(&SessionClientMeta {
        peer: "127.0.0.1:1".into(),
        peer_port: 1,
        pid: 2,
        command: "demo".into(),
        machine_fp: None,
    }))
    .unwrap();
    assert!(preamble.contains("client:"));
    assert!(preamble.contains("peer_port: 1"));
    assert!(preamble.contains("demo"));
}
