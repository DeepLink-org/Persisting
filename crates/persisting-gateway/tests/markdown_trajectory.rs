//! Capture-owned metadata and semantic live upsert tests.

use persisting_gateway::projection::markdown_trajectory::upsert_storyline_turn;
use persisting_gateway::session::client::{
    write_session_client_meta, SessionClientMeta, SESSION_CLIENT_META_FILENAME,
};
use persisting_pchronicle::document::parse_agenticmd;
use persisting_pchronicle::model::{StorylineDocument, StorylineTurn};
use serde_json::json;

fn turn(id: i64, source: &str, body: &str, draft: bool) -> StorylineTurn {
    StorylineTurn {
        id,
        kind: Some(if draft {
            "llm.response.stream".into()
        } else if source == "user" {
            "llm.request".into()
        } else {
            "llm.response".into()
        }),
        timestamp: None,
        source: source.into(),
        message: json!(body),
        reasoning_content: None,
        reasoning_effort: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        model_name: None,
        llm_call_count: (source == "agent").then_some(1),
        is_copied_context: None,
        latency_ms: None,
        ttft_ms: None,
        extra: None,
    }
}

#[test]
fn new_document_includes_session_client_metadata() {
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

    let path = session_dir.join("sess-1.md");
    let story = StorylineDocument::new("sess-1", "demo-agent");
    upsert_storyline_turn(&path, &story, "call-1", &turn(1, "agent", "hi", false)).unwrap();

    let parsed = parse_agenticmd(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.turns[0].message, json!("hi"));
    assert_eq!(
        parsed.agent.extra.as_ref().unwrap()["client"]["peer_port"],
        54321
    );
    assert_eq!(
        parsed.agent.extra.as_ref().unwrap()["client"]["command"],
        "claude --model deepseek"
    );
}

#[test]
fn live_upsert_replaces_draft_by_edit_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sess.md");
    let story = StorylineDocument::new("sess-1", "demo-agent");
    assert!(
        !upsert_storyline_turn(&path, &story, "call-1", &turn(1, "agent", "draft", true),).unwrap()
    );
    assert!(upsert_storyline_turn(
        &path,
        &story,
        "call-1",
        &turn(1, "agent", "complete", false),
    )
    .unwrap());

    let parsed = parse_agenticmd(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.turns.len(), 1);
    assert_eq!(parsed.turns[0].message, json!("complete"));
    assert_eq!(parsed.turns[0].kind.as_deref(), Some("llm.response"));
    assert!(parsed.turns[0]
        .extra
        .as_ref()
        .and_then(|extra| extra.get("call_id"))
        .is_none());
}
