//! Golden AgenticMD semantics through the public Storyline API.

use persisting_gateway::projection::dialogue::capture_record_to_storyline_turn;
use persisting_gateway::sink::{llm_request_record, llm_response_record};
use persisting_gateway::Call;
use persisting_pchronicle::{encode_agenticmd, parse_agenticmd, StorylineDocument};
use serde_json::json;

fn demo_storyline() -> StorylineDocument {
    let call = Call {
        call_id: "call-demo-1".into(),
        trace_id: "trace-demo-1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut req = llm_request_record(
        Some("demo-run-001".into()),
        Some("demo-agent".into()),
        "deepseek-chat",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"你好"}]}),
    );
    req.seq = 1;
    req.call_id = Some("call-demo-1".into());
    req.timestamp = Some("2026-01-01T00:00:00Z".into());
    let mut resp = llm_response_record(
        Some("demo-run-001".into()),
        Some("demo-agent".into()),
        200,
        &json!({
            "choices":[{"message":{"role":"assistant","content":"你好！有什么可以帮你的？"}}],
            "usage":{"prompt_tokens":12,"completion_tokens":18,"total_tokens":30}
        }),
        false,
        &call,
    );
    resp.seq = 2;
    resp.timestamp = Some("2026-01-01T00:00:01Z".into());

    let mut story = StorylineDocument::new("demo-run-001", "demo-agent");
    story.turns = vec![
        capture_record_to_storyline_turn(&req).unwrap(),
        capture_record_to_storyline_turn(&resp).unwrap(),
    ];
    story
}

#[test]
fn generated_agenticmd_preserves_golden_storyline_semantics() {
    let story = demo_storyline();
    let encoded = encode_agenticmd(&story).unwrap();
    assert_eq!(parse_agenticmd(&encoded).unwrap(), story);
}

#[test]
fn checked_in_legacy_golden_remains_readable() {
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agenticmd/demo-run-001.md");
    let story = parse_agenticmd(&std::fs::read_to_string(&fixture).unwrap()).unwrap();
    assert_eq!(story.session_id, "demo-run-001");
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].message, json!("你好"));
    assert_eq!(story.turns[1].message, json!("你好！有什么可以帮你的？"));
}
