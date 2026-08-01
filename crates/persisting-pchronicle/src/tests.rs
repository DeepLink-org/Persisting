use serde_json::json;

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::ingest::{ingest_trajectory, reconstruct_trajectory, split_trajectory};
use crate::store::MemoryChronicleStore;
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
fn typed_events_roundtrip_through_ron_transport() {
    let record = crate::EventRecord {
        seq: 7,
        source: "test".into(),
        kind: "http.request".into(),
        timestamp: None,
        session_id: Some("sess-1".into()),
        agent_id: Some("agent".into()),
        parent_uuid: None,
        trace_id: None,
        call_id: Some("call-1".into()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: None,
        payload: json!({"http": {"method": "POST", "path": "/v1/messages"}}),
    };
    let lines = crate::encode_event_lines(std::slice::from_ref(&record)).unwrap();
    let decoded = crate::decode_event_lines(&lines).unwrap();
    assert_eq!(decoded, vec![record]);
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
fn chronicle_format_names_are_canonical_only() {
    use crate::ChronicleFormat;
    use std::str::FromStr;
    assert_eq!(
        ChronicleFormat::from_str("storyline").unwrap(),
        ChronicleFormat::Storyline
    );
    assert!(ChronicleFormat::from_str("storyline/v1").is_err());
    assert!(ChronicleFormat::Storyline.is_hub());
    assert_eq!(
        ChronicleFormat::from_str("events").unwrap(),
        ChronicleFormat::Events
    );
    assert!(ChronicleFormat::from_str("lance").is_err());
    assert!(ChronicleFormat::Events.is_lance_only());
    assert_eq!(
        ChronicleFormat::from_str("agenticmd").unwrap(),
        ChronicleFormat::Agenticmd
    );
    assert!(ChronicleFormat::from_str("md").is_err());
    assert_eq!(
        ChronicleFormat::from_str("openai_msg").unwrap(),
        ChronicleFormat::OpenaiMsg
    );
    assert!(ChronicleFormat::from_str("session_steps").is_err());
    assert_eq!(
        ChronicleFormat::from_str("atif").unwrap(),
        ChronicleFormat::Atif
    );
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
    let story = crate::convert::events_to_storyline(&doc).unwrap();
    assert_eq!(story.session_id, "s1");
    assert!(!story.turns.is_empty());
    let agent = story.turns.iter().find(|t| t.source == "agent").unwrap();
    assert_eq!(agent.message, json!("hello"));
    assert_eq!(agent.latency_ms, Some(1000));
    assert_eq!(agent.ttft_ms, Some(120));
}

#[test]
fn http_event_aliases_project_to_storyline() {
    let doc = crate::EventsDocument::new(vec![
        crate::EventRecord {
            seq: 0,
            source: "capture".into(),
            kind: "http.request".into(),
            timestamp: None,
            session_id: Some("http-session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-http".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"user_content": "hello", "model": "m"}),
        },
        crate::EventRecord {
            seq: 1,
            source: "capture".into(),
            kind: "http.response".into(),
            timestamp: None,
            session_id: Some("http-session".into()),
            agent_id: Some("agent".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-http".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"assistant_content": "world"}),
        },
    ]);
    let story = crate::convert::events_to_storyline(&doc).unwrap();
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].message, json!("hello"));
    assert_eq!(story.turns[1].message, json!("world"));
}

#[test]
fn events_string_convert_is_lance_only_error() {
    use crate::convert::{convert, from_storyline, into_storyline};
    use crate::formats::events::events_lance_only_message;
    use crate::ChronicleFormat;
    let err = into_storyline(ChronicleFormat::Events, "[]").unwrap_err();
    assert!(
        err.to_string().contains("Lance-only")
            || err
                .to_string()
                .contains(events_lance_only_message().split(';').next().unwrap())
    );
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

    use crate::encode_agenticmd_block;
    let one = encode_agenticmd_block(&doc.blocks[0]).unwrap();
    assert!(text.contains(one.trim_end()));
}

#[test]
fn agenticmd_strict_rejects_garbage_and_unclosed_frontmatter() {
    use crate::{parse_agenticmd_blocks_with_spans, parse_agenticmd_document};

    let unclosed = "---\nformat: persisting:1.0\n";
    let err = parse_agenticmd_document(unclosed).unwrap_err();
    assert!(
        err.to_string().contains("unclosed YAML frontmatter"),
        "{err}"
    );

    let garbage = "---\nformat: persisting:1.0\n---\n\nnot a block\n";
    let err = parse_agenticmd_document(garbage).unwrap_err();
    assert!(
        err.to_string().contains("expected `<!-- persisting:block"),
        "{err}"
    );

    let spans = parse_agenticmd_blocks_with_spans("---\nformat: persisting:1.0\n---\n\n").unwrap();
    assert!(spans.is_empty());
}

#[test]
fn agenticmd_body_byte_offset_matches_split() {
    use crate::agenticmd_body_byte_offset;
    assert_eq!(agenticmd_body_byte_offset("no-fm").unwrap(), 0);
    let doc = "---\nformat: persisting:1.0\n---\n\nbody";
    let off = agenticmd_body_byte_offset(doc).unwrap();
    assert_eq!(&doc[off..], "\nbody");
    let err = agenticmd_body_byte_offset("---\nno close\n").unwrap_err();
    assert!(err.to_string().contains("unclosed YAML frontmatter"));
}

#[test]
fn encode_agenticmd_preamble_preserves_nested_mapping() {
    use crate::{
        agenticmd_body_byte_offset, encode_agenticmd_preamble, AGENTICMD_BLOCK_LAYOUT,
        AGENTICMD_FRONTMATTER_FORMAT,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    struct Fm<'a> {
        format: &'a str,
        block: &'a str,
        client: Client,
    }
    #[derive(Serialize)]
    struct Client {
        peer_port: u16,
        command: String,
    }

    let preamble = encode_agenticmd_preamble(&Fm {
        format: AGENTICMD_FRONTMATTER_FORMAT,
        block: AGENTICMD_BLOCK_LAYOUT,
        client: Client {
            peer_port: 9,
            command: "x".into(),
        },
    })
    .unwrap();
    assert!(preamble.contains("peer_port: 9"));
    let off = agenticmd_body_byte_offset(&preamble).unwrap();
    assert!(
        preamble[off..].trim().is_empty(),
        "body after preamble should be blank"
    );
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

#[test]
fn convert_storyline_agenticmd_preserves_dialogue_and_timing() {
    use crate::convert::convert;
    use crate::ChronicleFormat;
    let story = r#"{
      "spec": "storyline/v1",
      "session": "sess-md",
      "agent": { "id": "agent-md", "name": "demo" },
      "turns": [
        { "id": 1, "src": "user", "msg": "ask me" },
        { "id": 2, "src": "agent", "msg": "answer", "latency_ms": 42, "ttft_ms": 7, "model": "m1" }
      ]
    }"#;
    let md = convert(
        ChronicleFormat::Storyline,
        ChronicleFormat::Agenticmd,
        story,
    )
    .unwrap();
    assert!(md.contains("format: persisting:1.0"));
    assert!(md.contains("ask me"));
    assert!(md.contains("answer"));
    assert!(md.contains("latency_ms"));
    let back = convert(ChronicleFormat::Agenticmd, ChronicleFormat::Storyline, &md).unwrap();
    let v: serde_json::Value = serde_json::from_str(&back).unwrap();
    assert_eq!(v["session"], "sess-md");
    assert_eq!(v["agent"]["id"], "agent-md");
    let turns = v["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["src"], "user");
    assert_eq!(turns[0]["msg"], "ask me");
    assert_eq!(turns[1]["src"], "agent");
    assert_eq!(turns[1]["msg"], "answer");
    assert_eq!(turns[1]["latency_ms"], 42);
    assert_eq!(turns[1]["ttft_ms"], 7);
}

#[test]
fn convert_agenticmd_storyline_preserves_call_id_and_seq() {
    use crate::convert::{agenticmd_to_storyline, storyline_to_agenticmd};
    use crate::formats::agenticmd::{AgenticmdBlock, AgenticmdDocument, AgenticmdHeader};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn block(role: &str, kind: &str, call_id: &str, seq: u64, body: &str) -> AgenticmdBlock {
        let mut fields = BTreeMap::new();
        fields.insert("role".into(), json!(role));
        fields.insert("kind".into(), json!(kind));
        fields.insert("call_id".into(), json!(call_id));
        fields.insert("seq".into(), json!(seq));
        fields.insert("turn".into(), json!(seq / 2 + 1));
        AgenticmdBlock {
            header: AgenticmdHeader {
                type_name: "markdown".into(),
                length: body.len(),
                fields,
            },
            body: body.into(),
        }
    }

    let doc = AgenticmdDocument {
        format: "agenticmd".into(),
        frontmatter_format: "persisting:1.0".into(),
        session_id: Some("s-cid".into()),
        agent_id: Some("a-cid".into()),
        frontmatter: BTreeMap::new(),
        blocks: vec![
            block("user", "llm.request", "c-42", 0, "hello"),
            block("assistant", "llm.response", "c-42", 1, "world"),
        ],
    };
    let story = agenticmd_to_storyline(&doc).unwrap();
    assert_eq!(story.turns.len(), 2);
    assert_eq!(
        story.turns[0].extra.as_ref().unwrap()["call_id"],
        json!("c-42")
    );
    assert_eq!(story.turns[0].extra.as_ref().unwrap()["seq"], json!(0));
    assert_eq!(
        story.turns[1].extra.as_ref().unwrap()["call_id"],
        json!("c-42")
    );
    assert_eq!(story.turns[1].extra.as_ref().unwrap()["seq"], json!(1));
    assert_eq!(story.turns[0].kind.as_deref(), Some("llm.request"));
    assert_eq!(story.turns[1].kind.as_deref(), Some("llm.response"));

    let back = storyline_to_agenticmd(&story).unwrap();
    assert_eq!(back.blocks.len(), 2);
    assert_eq!(
        back.blocks[0].header.fields.get("call_id"),
        Some(&json!("c-42"))
    );
    assert_eq!(back.blocks[0].header.fields.get("seq"), Some(&json!(0)));
    assert_eq!(
        back.blocks[1].header.fields.get("call_id"),
        Some(&json!("c-42"))
    );
    assert_eq!(back.blocks[1].header.fields.get("seq"), Some(&json!(1)));
    assert_eq!(back.blocks[0].header.type_name, "markdown");
    assert_eq!(
        back.blocks[0].header.fields.get("kind"),
        Some(&json!("llm.request"))
    );
}

#[test]
fn events_storyline_roundtrip_preserves_call_id_and_seq() {
    use crate::convert::{events_to_storyline, storyline_to_events};
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            seq: 10,
            source: "proxy".into(),
            kind: "llm.request".into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            session_id: Some("s-seq".into()),
            agent_id: Some("a-seq".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-x".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"messages":[{"role":"user","content":"ping"}]}),
        },
        EventRecord {
            seq: 11,
            source: "proxy".into(),
            kind: "llm.response".into(),
            timestamp: Some("2026-01-01T00:00:01Z".into()),
            session_id: Some("s-seq".into()),
            agent_id: Some("a-seq".into()),
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-x".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"content":"pong"}),
        },
    ]);
    let story = events_to_storyline(&doc).unwrap();
    assert_eq!(
        story.turns[0].extra.as_ref().unwrap()["call_id"],
        json!("call-x")
    );
    assert_eq!(story.turns[0].extra.as_ref().unwrap()["seq"], json!(10));
    assert_eq!(
        story.turns[1].extra.as_ref().unwrap()["call_id"],
        json!("call-x")
    );
    assert_eq!(story.turns[1].extra.as_ref().unwrap()["seq"], json!(11));

    let back = storyline_to_events(&story).unwrap();
    assert_eq!(back.events[0].call_id.as_deref(), Some("call-x"));
    assert_eq!(back.events[0].seq, 10);
    assert_eq!(back.events[1].call_id.as_deref(), Some("call-x"));
    assert_eq!(back.events[1].seq, 11);
}

#[test]
fn convert_openai_msg_storyline_roundtrip_messages() {
    use crate::convert::convert;
    use crate::ChronicleFormat;
    let raw = r#"{
      "format_version": 1,
      "session_id": "s-om",
      "session_dir": "s-om",
      "agent_id": "a-om",
      "run_bucket": "b1",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s-om",
        "step_id": 0,
        "job_id": "",
        "agent_id": "a-om",
        "group_id": "",
        "env_name": "",
        "llm_model": "gpt-4o",
        "step_reward": 1.0,
        "reward": 1.0,
        "is_terminal": true,
        "is_truncated": false,
        "is_session_completed": true,
        "is_trainable": true,
        "created_at": "2026-07-29T00:00:00Z",
        "messages": [{"role":"user","content":"ping"}],
        "response": {"role":"assistant","content":"pong"},
        "run_bucket": "b1",
        "call_id": "c1"
      }]
    }"#;
    let story = convert(ChronicleFormat::OpenaiMsg, ChronicleFormat::Storyline, raw).unwrap();
    let v: serde_json::Value = serde_json::from_str(&story).unwrap();
    assert_eq!(v["session"], "s-om");
    assert_eq!(v["turns"][0]["msg"], "ping");
    assert_eq!(v["turns"][1]["msg"], "pong");

    let back = convert(
        ChronicleFormat::Storyline,
        ChronicleFormat::OpenaiMsg,
        &story,
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&back).unwrap();
    assert_eq!(doc["session_id"], "s-om");
    assert!(!doc["session_steps"].as_array().unwrap().is_empty());
}

#[test]
fn events_storyline_roundtrip_preserves_call_dialogue() {
    use crate::convert::{events_to_storyline, storyline_to_events};
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            seq: 0,
            source: "proxy".into(),
            kind: "llm.request".into(),
            timestamp: Some("2026-01-01T00:00:00Z".into()),
            session_id: Some("s-ev".into()),
            agent_id: Some("a-ev".into()),
            parent_uuid: None,
            trace_id: Some("t1".into()),
            call_id: Some("c1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"model":"m","messages":[{"role":"user","content":"hi there"}]}),
        },
        EventRecord {
            seq: 1,
            source: "proxy".into(),
            kind: "llm.response".into(),
            timestamp: Some("2026-01-01T00:00:01Z".into()),
            session_id: Some("s-ev".into()),
            agent_id: Some("a-ev".into()),
            parent_uuid: None,
            trace_id: Some("t1".into()),
            call_id: Some("c1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({"content":"hello back", "latency_ms": 900, "ttft_ms": 50}),
        },
    ]);
    let story = events_to_storyline(&doc).unwrap();
    assert_eq!(story.session_id, "s-ev");
    assert_eq!(story.agent.id, "a-ev");
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].source, "user");
    assert_eq!(story.turns[0].message, json!("hi there"));
    assert_eq!(story.turns[1].source, "agent");
    assert_eq!(story.turns[1].message, json!("hello back"));
    assert_eq!(story.turns[1].latency_ms, Some(900));
    assert_eq!(story.turns[1].ttft_ms, Some(50));

    let back = storyline_to_events(&story).unwrap();
    assert_eq!(back.session_id.as_deref(), Some("s-ev"));
    assert!(!back.events.is_empty());
    let again = events_to_storyline(&back).unwrap();
    assert_eq!(again.session_id, "s-ev");
    let user = again.turns.iter().find(|t| t.source == "user").unwrap();
    let agent = again.turns.iter().find(|t| t.source == "agent").unwrap();
    assert_eq!(user.message, json!("hi there"));
    assert_eq!(agent.message, json!("hello back"));
}

#[test]
fn convert_identity_and_cross_atif_storyline() {
    use crate::convert::convert;
    use crate::ChronicleFormat;
    let atif = serde_json::to_string_pretty(&sample_traj()).unwrap();
    let same = convert(ChronicleFormat::Atif, ChronicleFormat::Atif, &atif).unwrap();
    assert_eq!(same, atif);

    let story = convert(ChronicleFormat::Atif, ChronicleFormat::Storyline, &atif).unwrap();
    let back = convert(ChronicleFormat::Storyline, ChronicleFormat::Atif, &story).unwrap();
    let traj: crate::AtifTrajectory = serde_json::from_str(&back).unwrap();
    assert_eq!(traj.effective_session_id().unwrap(), "sess-1");
    assert_eq!(traj.steps.len(), 2);
    assert_eq!(traj.steps[0].source, "user");
    assert_eq!(traj.steps[1].source, "agent");
    assert_eq!(
        traj.steps[1].tool_calls.as_ref().unwrap()[0].function_name,
        "financial_search"
    );
}

#[test]
fn storyline_to_events_assigns_call_id_for_paired_turns() {
    use crate::convert::{events_to_storyline, storyline_to_events};
    use crate::formats::storyline::{StorylineAgent, StorylineDocument, StorylineTurn};
    use serde_json::json;
    let story = StorylineDocument {
        schema_version: "storyline/v1".into(),
        run_id: None,
        session_id: "s-pair".into(),
        agent: StorylineAgent {
            id: "a1".into(),
            name: Some("demo".into()),
            version: None,
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: None,
        continued_trajectory_ref: None,
        extra: None,
        turns: vec![
            StorylineTurn {
                id: 1,
                kind: None,
                timestamp: None,
                source: "user".into(),
                message: json!("hello"),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
            },
            StorylineTurn {
                id: 2,
                kind: None,
                timestamp: None,
                source: "agent".into(),
                message: json!("world"),
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: Some(1),
                is_copied_context: None,
                latency_ms: Some(10),
                ttft_ms: None,
                extra: None,
            },
        ],
    };
    let doc = storyline_to_events(&story).unwrap();
    assert_eq!(doc.events.len(), 2);
    assert_eq!(doc.events[0].call_id.as_deref(), Some("turn-1"));
    assert_eq!(doc.events[1].call_id.as_deref(), Some("turn-1"));
    let back = events_to_storyline(&doc).unwrap();
    assert_eq!(back.turns.len(), 2);
    assert_eq!(back.turns[0].message, json!("hello"));
    assert_eq!(back.turns[1].message, json!("world"));
}

#[test]
fn convert_atif_to_agenticmd_keeps_user_agent_text() {
    use crate::convert::convert;
    use crate::ChronicleFormat;
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    let md = convert(ChronicleFormat::Atif, ChronicleFormat::Agenticmd, &atif).unwrap();
    assert!(md.contains("What is the price of GOOGL?"));
    assert!(md.contains("I will search."));
    let story = convert(ChronicleFormat::Agenticmd, ChronicleFormat::Storyline, &md).unwrap();
    let v: serde_json::Value = serde_json::from_str(&story).unwrap();
    assert_eq!(v["session"], "sess-1");
    assert!(v["turns"].as_array().unwrap().len() >= 2);
}
