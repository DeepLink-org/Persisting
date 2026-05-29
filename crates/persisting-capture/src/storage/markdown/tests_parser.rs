use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        let encoded = encode_block_with_header(block_header("user", "llm.request"), b"a").unwrap()
            + &encode_block_with_header(block_header("assistant", "llm.response"), b"b").unwrap();
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
        let preamble = format_document_preamble(Some(&crate::session_client::SessionClientMeta {
            peer: "127.0.0.1:40000".into(),
            peer_port: 40000,
            pid: 42,
            command: "python3 agent.py".into(),
            machine_fp: None,
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
