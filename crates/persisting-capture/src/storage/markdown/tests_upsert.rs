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
    upsert_block_by_call_id(&path, "call-1", block_with_call("call-1", "user", b"hello")).unwrap();
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
    upsert_block_by_call_id(&path, "call-a", block_with_call("call-a", "user", b"req-a")).unwrap();
    upsert_block_by_call_id(
        &path,
        "call-a",
        block_with_call("call-a", "assistant", b"draft-a"),
    )
    .unwrap();
    // call-B request arrives before call-A response completes
    upsert_block_by_call_id(&path, "call-b", block_with_call("call-b", "user", b"req-b")).unwrap();
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
