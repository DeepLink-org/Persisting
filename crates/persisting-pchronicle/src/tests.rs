use serde_json::json;

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::ingest::{ingest_trajectory, reconstruct_trajectory, split_trajectory};
use crate::store::{FsChronicleStore, MemoryChronicleStore};
use crate::view::{atif_trajectory_sql_ddl, AtifTrajectoryView, ATIF_TRAJECTORY_VIEW};

fn sample_traj() -> AtifTrajectory {
    AtifTrajectory {
        schema_version: "ATIF-v1.7".into(),
        session_id: Some("sess-1".into()),
        trajectory_id: Some("traj-1".into()),
        agent: AtifAgent {
            name: "harbor-agent".into(),
            version: "1.0.0".into(),
            model_name: Some("gemini-2.5-flash".into()),
            tool_definitions: None,
            extra: None,
        },
        notes: Some("unit test".into()),
        final_metrics: Some(json!({"total_steps": 2})),
        continued_trajectory_ref: None,
        extra: None,
        subagent_trajectories: None,
        steps: vec![
            AtifStep {
                step_id: 1,
                timestamp: Some("2025-10-11T10:30:00Z".into()),
                source: "user".into(),
                model_name: None,
                reasoning_effort: None,
                message: json!("What is the price of GOOGL?"),
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            },
            AtifStep {
                step_id: 2,
                timestamp: Some("2025-10-11T10:30:02Z".into()),
                source: "agent".into(),
                model_name: Some("gemini-2.5-flash".into()),
                reasoning_effort: Some(json!("medium")),
                message: json!("I will search."),
                reasoning_content: Some("Need price and volume.".into()),
                tool_calls: Some(vec![
                    AtifToolCall {
                        tool_call_id: "call_price_1".into(),
                        function_name: "financial_search".into(),
                        arguments: json!({"ticker":"GOOGL","metric":"price"}),
                        extra: Some(json!({"duration_ms": 42})),
                    },
                    AtifToolCall {
                        tool_call_id: "call_volume_2".into(),
                        function_name: "financial_search".into(),
                        arguments: json!({"ticker":"GOOGL","metric":"volume"}),
                        extra: Some(json!({"duration_ms": 37})),
                    },
                ]),
                observation: Some(AtifObservation {
                    results: vec![
                        json!({"source_call_id":"call_price_1","content":"$185.35"}),
                        json!({"source_call_id":"call_volume_2","content":"1.5M"}),
                    ],
                }),
                metrics: Some(json!({
                    "prompt_tokens": 520,
                    "completion_tokens": 80,
                    "latency_ms": 1850,
                    "ttft_ms": 210
                })),
                extra: None,
                llm_call_count: Some(1),
                is_copied_context: None,
            },
        ],
    }
}

#[test]
fn split_creates_three_tables_with_keys() {
    let split = split_trajectory(&sample_traj()).unwrap();
    assert_eq!(split.session.session_id, "sess-1");
    assert_eq!(split.steps.len(), 2);
    assert_eq!(split.tool_calls.len(), 2);
    assert_eq!(split.tool_calls[0].session_id, "sess-1");
    assert_eq!(split.tool_calls[0].step_id, 2);
    assert_eq!(split.tool_calls[0].tool_call_id, "call_price_1");
    assert_eq!(split.tool_calls[1].tool_call_id, "call_volume_2");
}

#[test]
fn memory_roundtrip_and_view_expands_tool_calls() {
    let mut store = MemoryChronicleStore::new();
    let id = ingest_trajectory(&mut store, &sample_traj()).unwrap();
    assert_eq!(id, "sess-1");

    let rebuilt = reconstruct_trajectory(&store, &id).unwrap();
    assert_eq!(rebuilt.steps.len(), 2);
    assert_eq!(
        rebuilt.steps[1]
            .tool_calls
            .as_ref()
            .map(|c| c.len())
            .unwrap_or(0),
        2
    );

    let view = AtifTrajectoryView::new(&store);
    let rows = view.query(Some("sess-1")).unwrap();
    // step1 (no tool) + step2 (2 tools) = 3 rows
    assert_eq!(rows.len(), 3);
    assert!(rows[0].tool_call_id.is_none());
    assert_eq!(rows[1].tool_call_id.as_deref(), Some("call_price_1"));
    assert_eq!(rows[2].tool_call_id.as_deref(), Some("call_volume_2"));
    assert_eq!(rows[1].function_name.as_deref(), Some("financial_search"));
    assert_eq!(rows[1].agent_name, "harbor-agent");
}

#[test]
fn fs_store_persists_jsonl_tables() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = FsChronicleStore::open(dir.path()).unwrap();
    ingest_trajectory(&mut store, &sample_traj()).unwrap();

    assert!(dir.path().join("sessions.jsonl").exists());
    assert!(dir.path().join("steps.jsonl").exists());
    assert!(dir.path().join("tool_calls.jsonl").exists());

    let reopened = FsChronicleStore::open(dir.path()).unwrap();
    let rebuilt = reconstruct_trajectory(&reopened, "sess-1").unwrap();
    assert_eq!(rebuilt.agent.name, "harbor-agent");
    assert_eq!(rebuilt.steps[1].tool_calls.as_ref().unwrap().len(), 2);

    let rows = AtifTrajectoryView::new(&reopened)
        .query(None)
        .unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn sql_ddl_mentions_three_tables_and_view_name() {
    let ddl = atif_trajectory_sql_ddl();
    assert!(ddl.contains(ATIF_TRAJECTORY_VIEW));
    assert!(ddl.contains("FROM sessions"));
    assert!(ddl.contains("JOIN steps"));
    assert!(ddl.contains("LEFT JOIN tool_calls"));
}


#[test]
fn chronicle_format_aliases() {
    use crate::ChronicleFormat;
    use std::str::FromStr;
    assert_eq!(ChronicleFormat::from_str("storyline").unwrap(), ChronicleFormat::Storyline);
    assert_eq!(ChronicleFormat::from_str("storyline/v1").unwrap(), ChronicleFormat::Storyline);
    assert!(ChronicleFormat::Storyline.is_hub());
    assert_eq!(ChronicleFormat::from_str("events").unwrap(), ChronicleFormat::Events);
    assert_eq!(ChronicleFormat::from_str("lance").unwrap(), ChronicleFormat::Events);
    assert!(ChronicleFormat::Events.is_lance_only());
    assert_eq!(ChronicleFormat::from_str("agenticmd").unwrap(), ChronicleFormat::Agenticmd);
    assert_eq!(ChronicleFormat::from_str("md").unwrap(), ChronicleFormat::Agenticmd);
    assert_eq!(ChronicleFormat::from_str("openai_msg").unwrap(), ChronicleFormat::OpenaiMsg);
    assert_eq!(ChronicleFormat::from_str("session_steps").unwrap(), ChronicleFormat::OpenaiMsg);
    assert_eq!(ChronicleFormat::from_str("atif").unwrap(), ChronicleFormat::Atif);
}

#[test]
fn atif_storyline_hub_roundtrip() {
    use crate::convert::{from_storyline, into_storyline};
    use crate::ChronicleFormat;
    let raw = serde_json::to_string_pretty(&sample_traj()).unwrap();
    let story = into_storyline(ChronicleFormat::Atif, &raw).unwrap();
    assert_eq!(story.session_id, "sess-1");
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].id, 1);
    assert_eq!(story.turns[0].source, "user");
    assert_eq!(story.turns[1].id, 2);
    assert_eq!(story.turns[1].source, "agent");
    // LLM reply lives in message (ATIF-identical)
    assert_eq!(story.turns[1].message, serde_json::json!("I will search."));
    assert_eq!(story.turns[1].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(story.turns[1].latency_ms, Some(1850));
    assert_eq!(story.turns[1].ttft_ms, Some(210));
    assert_eq!(
        story.turns[1].tool_calls.as_ref().unwrap()[0].duration_ms,
        Some(42)
    );

    let out = from_storyline(ChronicleFormat::Atif, &story).unwrap();
    let back: crate::AtifTrajectory = serde_json::from_str(&out).unwrap();
    assert_eq!(back.effective_session_id().unwrap(), "sess-1");
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.steps[1].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(
        back.steps[1]
            .metrics
            .as_ref()
            .unwrap()
            .get("latency_ms")
            .and_then(|v| v.as_i64()),
        Some(1850)
    );
    assert_eq!(
        back.steps[1].tool_calls.as_ref().unwrap()[0]
            .extra
            .as_ref()
            .unwrap()
            .get("duration_ms")
            .and_then(|v| v.as_i64()),
        Some(42)
    );
}

#[test]
fn convert_peripheral_via_hub_only() {
    use crate::convert::convert;
    use crate::ChronicleFormat;
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    // atif → openai_msg must go through storyline (API guarantees hub path).
    let openai = convert(ChronicleFormat::Atif, ChronicleFormat::OpenaiMsg, &atif).unwrap();
    assert!(openai.contains("session_steps") || openai.contains("\"session_id\""));
    let back = convert(ChronicleFormat::OpenaiMsg, ChronicleFormat::Atif, &openai).unwrap();
    let traj: crate::AtifTrajectory = serde_json::from_str(&back).unwrap();
    assert_eq!(traj.effective_session_id().unwrap(), "sess-1");
    assert!(!traj.steps.is_empty());
}

#[test]
fn events_in_memory_to_storyline() {
    use crate::convert::events_to_storyline;
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            seq: 0,
            source: "proxy".into(),
            kind: "llm.request".into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            session_id: Some("s1".into()),
            agent_id: Some("a1".into()),
            parent_uuid: None,
            trace_id: Some("t1".into()),
            call_id: Some("c1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"model":"m","messages":[{"role":"user","content":"hi"}]}),
        },
        EventRecord {
            seq: 1,
            source: "proxy".into(),
            kind: "llm.response".into(),
            timestamp: Some("2026-01-01T00:00:01Z".into()),
            session_id: Some("s1".into()),
            agent_id: Some("a1".into()),
            parent_uuid: None,
            trace_id: Some("t1".into()),
            call_id: Some("c1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"content":"hello", "latency_ms": 1000, "ttft_ms": 120}),
        },
    ]);
    let story = events_to_storyline(&doc).unwrap();
    assert_eq!(story.session_id, "s1");
    assert!(!story.turns.is_empty());
    let agent = story.turns.iter().find(|t| t.source == "agent").unwrap();
    assert_eq!(agent.message, json!("hello"));
    assert_eq!(agent.latency_ms, Some(1000));
    assert_eq!(agent.ttft_ms, Some(120));
}

#[test]
fn events_string_convert_is_lance_only_error() {
    use crate::convert::{convert, from_storyline, into_storyline};
    use crate::formats::events::events_lance_only_message;
    use crate::ChronicleFormat;
    let err = into_storyline(ChronicleFormat::Events, "[]").unwrap_err();
    assert!(err.to_string().contains("Lance-only") || err.to_string().contains(events_lance_only_message().split(';').next().unwrap()));
    let story = crate::StorylineDocument::new("s", "a");
    assert!(from_storyline(ChronicleFormat::Events, &story).is_err());
    assert!(convert(ChronicleFormat::Atif, ChronicleFormat::Events, "{}").is_err());
    assert!(ChronicleFormat::Events.is_lance_only());
}


#[test]
fn export_events_jsonl_debug_roundtrip_via_test_parser() {
    use crate::formats::events::{
        export_events_jsonl, parse_events_jsonl_for_test, EventRecord, EventsDocument,
    };
    use serde_json::json;
    let events = vec![EventRecord {
        seq: 0,
        source: "proxy".into(),
        kind: "llm.request".into(),
        timestamp: Some("2026-01-01T00:00:00Z".into()),
        session_id: Some("s1".into()),
        agent_id: Some("a1".into()),
        parent_uuid: None,
        trace_id: Some("t1".into()),
        call_id: Some("c1".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({"model":"m","messages":[{"role":"user","content":"hi"}]}),
    }];
    let text = export_events_jsonl(&events).unwrap();
    let doc = parse_events_jsonl_for_test(&text).unwrap();
    assert_eq!(doc.format, EventsDocument::FORMAT_NAME);
    assert_eq!(doc.events.len(), 1);
    assert_eq!(doc.events[0].kind, "llm.request");
    assert_eq!(doc.session_id.as_deref(), Some("s1"));
}


#[test]
fn parse_agenticmd_document_roundtrip() {
    use crate::formats::agenticmd::{
        encode_agenticmd_document, parse_agenticmd_document, AgenticmdBlock, AgenticmdDocument,
        AgenticmdHeader,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    let mut fields = BTreeMap::new();
    fields.insert("role".into(), json!("user"));
    fields.insert("kind".into(), json!("dialogue"));
    let mut doc = AgenticmdDocument::new(vec![AgenticmdBlock {
        header: AgenticmdHeader {
            type_name: "text".into(),
            length: 0,
            fields,
        },
        body: "hello".into(),
    }]);
    doc.session_id = Some("sess-1".into());
    doc.agent_id = Some("agent-a".into());
    let text = encode_agenticmd_document(&doc).unwrap();
    assert!(text.contains("format: persisting:1.0"));
    assert!(text.contains("<!-- persisting:block:user"));
    let parsed = parse_agenticmd_document(&text).unwrap();
    assert_eq!(parsed.format, "agenticmd");
    assert_eq!(parsed.session_id.as_deref(), Some("sess-1"));
    assert_eq!(parsed.blocks.len(), 1);
    assert_eq!(parsed.blocks[0].body, "hello");
    assert_eq!(parsed.blocks[0].role(), Some("user"));
}

#[test]
fn parse_openai_msg_envelope() {
    use crate::formats::openai_msg::parse_openai_msg_document;
    let raw = r#"{
      "format_version": 1,
      "session_id": "s1",
      "session_dir": "s1",
      "agent_id": "a1",
      "run_bucket": "2026-07-29",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s1",
        "step_id": 0,
        "job_id": "",
        "agent_id": "a1",
        "group_id": "",
        "env_name": "",
        "llm_model": "gpt-4o",
        "step_reward": 0.0,
        "reward": 0.0,
        "is_terminal": true,
        "is_truncated": false,
        "is_session_completed": true,
        "is_trainable": true,
        "created_at": "2026-07-29T00:00:00Z",
        "messages": [{"role":"user","content":"ping"}],
        "response": {"role":"assistant","content":"pong"},
        "run_bucket": "2026-07-29",
        "call_id": "c1"
      }]
    }"#;
    let doc = parse_openai_msg_document(raw).unwrap();
    assert_eq!(doc.session_id, "s1");
    assert_eq!(doc.session_steps.len(), 1);
    let msgs = doc.session_steps[0].messages_value().unwrap();
    assert_eq!(msgs[0]["content"], "ping");
}

#[test]
fn detect_format_from_content_and_path() {
    use crate::formats::detect::{detect_format, detect_format_from_path};
    use crate::ChronicleFormat;
    use std::path::Path;
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/session_steps.json")),
        Some(ChronicleFormat::OpenaiMsg)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/events.lance")),
        Some(ChronicleFormat::Events)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/sess.md")),
        Some(ChronicleFormat::Agenticmd)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/storyline.json")),
        Some(ChronicleFormat::Storyline)
    );
    let atif = r#"{"schema_version":"ATIF-v1.7","session_id":"s","agent":{"name":"a","version":"1"},"steps":[]}"#;
    assert_eq!(
        detect_format(None, Some(atif)).unwrap(),
        Some(ChronicleFormat::Atif)
    );
    let story = r#"{"spec":"storyline/v1","session":"s","agent":{"id":"a"},"turns":[]}"#;
    assert_eq!(
        detect_format(None, Some(story)).unwrap(),
        Some(ChronicleFormat::Storyline)
    );
}

#[test]
fn openai_msg_preserves_user_and_llm_turns() {
    use crate::convert::into_storyline;
    use crate::ChronicleFormat;
    let raw = r#"{
      "format_version": 1,
      "session_id": "s1",
      "session_dir": "s1",
      "agent_id": "a1",
      "run_bucket": "2026-07-29",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s1",
        "step_id": 0,
        "job_id": "",
        "agent_id": "a1",
        "group_id": "",
        "env_name": "",
        "llm_model": "gpt-4o",
        "step_reward": 0.0,
        "reward": 0.0,
        "is_terminal": true,
        "is_truncated": false,
        "is_session_completed": true,
        "is_trainable": true,
        "created_at": "2026-07-29T00:00:00Z",
        "messages": [{"role":"user","content":"ping"}],
        "response": {"role":"assistant","content":"pong"},
        "run_bucket": "2026-07-29",
        "call_id": "c1"
      }]
    }"#;
    let story = into_storyline(ChronicleFormat::OpenaiMsg, raw).unwrap();
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].source, "user");
    assert_eq!(story.turns[0].message, serde_json::json!("ping"));
    assert_eq!(story.turns[1].source, "agent");
    assert_eq!(story.turns[1].message, serde_json::json!("pong"));
}


#[test]
fn storyline_wire_uses_short_keys() {
    use crate::convert::{from_storyline, into_storyline};
    use crate::ChronicleFormat;
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    let out = from_storyline(
        ChronicleFormat::Storyline,
        &into_storyline(ChronicleFormat::Atif, &atif).unwrap(),
    )
    .unwrap();
    assert!(out.contains(r#""spec""#));
    assert!(out.contains(r#""session""#));
    assert!(out.contains(r#""agent""#));
    assert!(out.contains(r#""src""#));
    assert!(out.contains(r#""msg""#));
    assert!(out.contains(r#""tool_calls""#));
    assert!(out.contains(r#""observation""#));
    assert!(out.contains(r#""metrics""#));
    assert!(out.contains(r#""final_metrics""#));
    assert!(!out.contains(r#""sv""#));
    assert!(!out.contains(r#""sid""#));
    assert!(!out.contains(r#""agt""#));
    assert!(!out.contains(r#""fm""#));
    assert!(!out.contains(r#""kids""#));
    assert!(!out.contains(r#""schema_version""#));
    assert!(!out.contains(r#""source""#));
    assert!(!out.contains(r#""message""#));
}
