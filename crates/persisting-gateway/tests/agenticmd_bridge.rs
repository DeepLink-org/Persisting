//! Capture → Storyline → AgenticMD bridge tests.

use persisting_gateway::projection::dialogue::capture_record_to_storyline_turn;
use persisting_gateway::projection::markdown_pipeline::MarkdownPipeline;
use persisting_gateway::record::EventRecord;
use persisting_gateway::sink::{llm_request_record, llm_response_record};
use persisting_gateway::Call;
use persisting_pchronicle::document::{decode_agenticmd, write_agenticmd_storyline};
use persisting_pchronicle::model::StorylineDocument;
use serde_json::json;

fn fixture() -> &'static str {
    include_str!("fixtures/agenticmd/demo-run-001.md")
}

fn materialize_records(path: &std::path::Path, records: &[EventRecord]) -> anyhow::Result<()> {
    let turns = MarkdownPipeline::storyline_turns_from_records(records)?;
    let mut story = StorylineDocument::new("s1", "gateway");
    story.turns = turns;
    write_agenticmd_storyline(path, &story)?;
    Ok(())
}

fn pair() -> [EventRecord; 2] {
    let call = Call {
        call_id: "c1".into(),
        trace_id: "t1".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    let mut req = llm_request_record(
        Some("s1".into()),
        Some("gateway".into()),
        "m",
        "/v1/chat/completions",
        &json!({"messages":[{"role":"user","content":"hi"}]}),
    );
    req.seq = 1;
    req.call_id = Some("c1".into());
    req.timestamp = Some("2026-01-01T00:00:00Z".into());
    let mut resp = llm_response_record(
        Some("s1".into()),
        Some("gateway".into()),
        200,
        &json!({"choices":[{"message":{"role":"assistant","content":"yo"}}]}),
        false,
        &call,
    );
    resp.seq = 2;
    resp.call_id = Some("c1".into());
    resp.timestamp = Some("2026-01-01T00:00:01Z".into());
    [req, resp]
}

#[test]
fn capture_turns_roundtrip_through_public_storyline_api() {
    let [req, resp] = pair();
    let mut story = StorylineDocument::new("s1", "gateway");
    story.turns = vec![
        capture_record_to_storyline_turn(&req).unwrap(),
        capture_record_to_storyline_turn(&resp).unwrap(),
    ];

    let encoded = persisting_pchronicle::document::encode_agenticmd(&story).unwrap();
    assert_eq!(decode_agenticmd(&encoded).unwrap(), story);
}

#[test]
fn legacy_fixture_without_authoritative_storyline_is_rejected() {
    let error = decode_agenticmd(fixture()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "missing authoritative Storyline metadata"
    );
}

#[test]
fn materialize_writes_storyline_parseable_markdown() {
    let records = pair();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess.md");
    materialize_records(&path, &records).unwrap();
    let story = decode_agenticmd(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(story.session_id, "s1");
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].message, json!("hi"));
    assert_eq!(story.turns[1].message, json!("yo"));
}
