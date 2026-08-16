//! Capture-owned client-meta preamble and live upsert seed.

use std::collections::BTreeMap;

use persisting_gateway::projection::markdown_trajectory::{
    format_document_preamble, upsert_block_by_call_id,
};
use persisting_gateway::session::client::{
    write_session_client_meta, SessionClientMeta, SESSION_CLIENT_META_FILENAME,
};
use persisting_pchronicle::{
    agenticmd_body_byte_offset, encode_agenticmd_block_validated,
    parse_agenticmd_document_validated as parse_document, read_agenticmd_blocks_from_file,
    AgenticmdBlock, AgenticmdHeader,
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

    let md_path = session_dir.join("sess-1.md");
    upsert_block_by_call_id(
        &md_path,
        "call-1",
        block_with_call("call-1", "assistant", "hi"),
    )
    .unwrap();

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
    assert!(raw.contains("persisting"));
    assert!(raw.contains("persisting:block:{speaker}"));
    let blocks = read_agenticmd_blocks_from_file(&path).unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].body, "hello");
}

#[test]
fn preamble_roundtrips_through_body_offset() {
    let preamble = format_document_preamble(None).unwrap();
    assert!(preamble.starts_with("---\n"));
    assert!(preamble.contains("format: persisting"));
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
