//! Golden AgenticMD document built through the production encoder.

use persisting_gateway::dialogue::capture_record_to_agenticmd_block;
use persisting_gateway::record::CaptureRecordExt;
use persisting_gateway::sink::{llm_request_record, llm_response_record};
use persisting_gateway::Call;
use persisting_pchronicle::{
    agenticmd_block_to_event_record, encode_agenticmd_block_validated,
    encode_agenticmd_session_frontmatter, parse_agenticmd_document_validated as parse_document,
    AgenticmdSessionFrontmatter,
};
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
    req.seq = 1;
    req.call_id = Some("call-demo-1".into());
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
    resp.seq = 2;
    resp.timestamp = Some("2026-01-01T00:00:01Z".into());

    let mut out = encode_agenticmd_session_frontmatter(&AgenticmdSessionFrontmatter {
        session: "demo-run-001".into(),
        agent: "demo-agent".into(),
        turns: 1,
        ..Default::default()
    })
    .unwrap();
    for rec in [req, resp] {
        let block = capture_record_to_agenticmd_block(&rec).unwrap();
        out.push_str(&encode_agenticmd_block_validated(&block).unwrap());
    }
    out
}

#[test]
fn demo_run_001_matches_golden_fixture() {
    let built = build_demo_document();
    if std::env::var("WRITE_AGENTICMD_GOLDEN").is_ok() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/agenticmd/demo-run-001.md");
        std::fs::write(&fixture, &built).unwrap();
        let example = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/trajectory-agenticmd/demo-agent/demo-run-001/demo-run-001.md");
        std::fs::write(example, &built).unwrap();
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agenticmd/demo-run-001.md");
    let golden = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("read {}: {e}", fixture.display()));
    assert_eq!(
        built, golden,
        "regenerate: WRITE_AGENTICMD_GOLDEN=1 cargo test -p persisting-gateway --test agenticmd_golden demo_run_001_matches_golden_fixture"
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

    let mut block = blocks[1].clone();
    block
        .body
        .push_str("\n<!-- persisting:subagent-self agent-abc.md -->\n");
    block.header.length = block.body.len();
    let rec = agenticmd_block_to_event_record(&block).unwrap();
    let content = rec.visible_assistant_text().unwrap_or_default();
    assert!(!content.contains("persisting:subagent"));
    assert!(content.contains("你好"));
}
