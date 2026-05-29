//! Golden TLV markdown document built only via [`encode_block_with_header`].

use persisting_capture::dialogue::{block_to_capture_record, capture_record_to_block};
use persisting_capture::markdown_trajectory::{
    encode_block_with_header, format_document_preamble, parse_document,
};
use persisting_capture::sink::{llm_request_record, llm_response_record};
use persisting_capture::Call;
use serde_json::json;

fn demo_call() -> Call {
    Call {
        call_id: "call-demo-1".into(),
        trace_id: "trace-demo-1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    }
}

const DEMO_TIMESTAMP: &str = "2026-01-01T00:00:00Z";

fn build_demo_document() -> String {
    let mut req = llm_request_record(
        Some("demo-run-001".into()),
        None,
        "deepseek-chat",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"你好"}]}),
    );
    req.timestamp = Some(DEMO_TIMESTAMP.into());
    let mut resp = llm_response_record(
        Some("demo-run-001".into()),
        None,
        200,
        &json!({
            "choices":[{"message":{"role":"assistant","content":"你好！有什么可以帮你的？"}}],
            "usage":{"prompt_tokens":12,"completion_tokens":18,"total_tokens":30}
        }),
        false,
        &demo_call(),
    );
    resp.call_id = Some("call-demo-1".into());
    resp.timestamp = Some(DEMO_TIMESTAMP.into());

    let note = persisting_capture::record::CaptureRecord {
        seq: 2,
        source: "markdown".into(),
        kind: "note".into(),
        timestamp: Some(DEMO_TIMESTAMP.into()),
        session_id: Some("demo-run-001".into()),
        agent_id: None,
        parent_uuid: None,
        trace_id: None,
        call_id: None,
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({
            "role": "note",
            "content": "本次会话由 `persisting capture run` 代理记录；元数据在块注释 JSON 中。"
        }),
    };

    let mut out = format_document_preamble(None).unwrap();
    for rec in [req, resp, note] {
        let (h, b) = capture_record_to_block(&rec).unwrap();
        out.push_str(&encode_block_with_header(h, &b).unwrap());
    }
    out
}

#[test]
fn demo_run_001_matches_golden_fixture() {
    let built = build_demo_document();
    if std::env::var("WRITE_TLV_GOLDEN").is_ok() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tlv/demo-run-001.md");
        std::fs::write(&fixture, &built).unwrap();
        let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/trajectory-tlv/demo-agent/demo-run-001/0001.md");
        std::fs::write(example, &built).unwrap();
    }
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tlv/demo-run-001.md");
    let golden = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("read {}: {e}", fixture.display()));
    assert_eq!(
        built, golden,
        "regenerate: WRITE_TLV_GOLDEN=1 cargo test -p persisting-capture --test tlv_golden demo_run_001_matches_golden_fixture"
    );
}

#[test]
fn demo_blocks_carry_v_field_and_strip_subagent_footer_on_import() {
    let built = build_demo_document();
    let blocks = parse_document(&built).unwrap();
    assert!(blocks[0]
        .header
        .fields
        .get("v")
        .and_then(|v| v.as_u64())
        .is_some());

    let mut body = blocks[1].body.clone();
    body.extend_from_slice(b"\n<!-- persisting:subagent-self agent-abc.md -->\n");
    let mut block = blocks[1].clone();
    block.body = body;
    let rec = block_to_capture_record(&block).unwrap();
    let content = rec.visible_assistant_text().unwrap_or_default();
    assert!(!content.contains("persisting:subagent"));
    assert!(content.contains("你好"));
}
