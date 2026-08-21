use serde_json::json;

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};

#[derive(Clone, Copy, PartialEq, Eq)]
enum TestFormat {
    Storyline,
    CanonicalEvent,
    AgenticMd,
    OpenaiMsg,
    Atif,
}

impl TestFormat {
    fn is_lance_only(self) -> bool {
        self == Self::CanonicalEvent
    }
}

fn into_storyline(format: TestFormat, input: &str) -> crate::Result<crate::StorylineDocument> {
    match format {
        TestFormat::Storyline => crate::formats::storyline::parse_storyline_document(input),
        TestFormat::CanonicalEvent => Err(lance_only_error()),
        TestFormat::AgenticMd => Ok(crate::document::decode_agenticmd(input)?),
        TestFormat::OpenaiMsg => {
            let value = serde_json::from_str(input)?;
            let mut stories = crate::formats::parse_openai_msg_corpus_value(&value, "corpus.json")?;
            if stories.len() != 1 {
                anyhow::bail!(
                    "{} document cannot represent {} storylines",
                    crate::DocumentFormat::OpenaiMsg,
                    stories.len()
                );
            }
            Ok(stories.remove(0))
        }
        TestFormat::Atif => {
            crate::convert::atif_to_storyline(&AtifTrajectory::from_json_str(input)?)
        }
    }
}

fn from_storyline(format: TestFormat, story: &crate::StorylineDocument) -> crate::Result<String> {
    match format {
        TestFormat::Storyline => story.to_json_string_pretty(),
        TestFormat::CanonicalEvent => Err(lance_only_error()),
        TestFormat::AgenticMd => crate::document::encode_agenticmd(story),
        TestFormat::OpenaiMsg => Ok(serde_json::to_string_pretty(
            &crate::formats::openai_corpus::synthesize_openai_msg_corpus_value(
                std::slice::from_ref(story),
            )?,
        )?),
        TestFormat::Atif => Ok(serde_json::to_string_pretty(
            &crate::convert::storyline_to_atif(story)?,
        )?),
    }
}

fn convert(from: TestFormat, to: TestFormat, input: &str) -> crate::Result<String> {
    if from == to {
        return if from.is_lance_only() {
            Err(lance_only_error())
        } else {
            Ok(input.to_string())
        };
    }
    from_storyline(to, &into_storyline(from, input)?)
}

fn lance_only_error() -> anyhow::Error {
    anyhow::anyhow!(crate::formats::events::events_lance_only_message())
}

fn sample_traj() -> AtifTrajectory {
    AtifTrajectory {
        schema_version: "ATIF-v1.7".into(),
        unknown: Default::default(),
        session_id: Some("sess-1".into()),
        trajectory_id: Some("traj-1".into()),
        agent: AtifAgent {
            name: "harbor-agent".into(),
            version: "1.0.0".into(),
            unknown: Default::default(),
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
                unknown: Default::default(),
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
                unknown: Default::default(),
                timestamp: Some("2025-10-11T10:30:02Z".into()),
                source: "agent".into(),
                model_name: Some("gemini-2.5-flash".into()),
                reasoning_effort: Some(json!("medium")),
                message: json!("I will search."),
                reasoning_content: Some("Need price and volume.".into()),
                tool_calls: Some(vec![
                    AtifToolCall {
                        tool_call_id: "call_price_1".into(),
                        unknown: Default::default(),
                        function_name: "financial_search".into(),
                        arguments: json!({"ticker":"GOOGL","metric":"price"}),
                        result: Some(json!({"price": 185.35})),
                        extra: Some(json!({"duration_ms": 42})),
                    },
                    AtifToolCall {
                        tool_call_id: "call_volume_2".into(),
                        unknown: Default::default(),
                        function_name: "financial_search".into(),
                        arguments: json!({"ticker":"GOOGL","metric":"volume"}),
                        result: None,
                        extra: Some(json!({"duration_ms": 37})),
                    },
                ]),
                observation: Some(AtifObservation {
                    results: vec![
                        json!({"source_call_id":"call_price_1","content":"$185.35"}),
                        json!({"source_call_id":"call_volume_2","content":"1.5M"}),
                    ],
                    unknown: Default::default(),
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
fn atif_storyline_hub_roundtrip() {
    let raw = serde_json::to_string_pretty(&sample_traj()).unwrap();
    let story = into_storyline(TestFormat::Atif, &raw).unwrap();
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

    let out = from_storyline(TestFormat::Atif, &story).unwrap();
    let back: crate::atif::AtifTrajectory = serde_json::from_str(&out).unwrap();
    assert_eq!(back.effective_session_id().unwrap(), "sess-1");
    assert_eq!(back.steps.len(), 2);
    assert_eq!(back.steps[1].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(
        back.steps[1].tool_calls.as_ref().unwrap()[0].result,
        Some(serde_json::json!({"price": 185.35}))
    );
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
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    // atif → openai_msg must go through storyline (API guarantees hub path).
    let openai = convert(TestFormat::Atif, TestFormat::OpenaiMsg, &atif).unwrap();
    assert!(openai.contains("session_steps") || openai.contains("\"session_id\""));
    let back = convert(TestFormat::OpenaiMsg, TestFormat::Atif, &openai).unwrap();
    let traj: crate::atif::AtifTrajectory = serde_json::from_str(&back).unwrap();
    assert_eq!(traj.effective_session_id().unwrap(), "sess-1");
    assert!(!traj.steps.is_empty());
}

#[test]
fn events_in_memory_to_storyline() {
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            identity: crate::EventIdentity::default(),
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
            identity: crate::EventIdentity::default(),
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
    let doc = crate::model::EventsDocument::new(vec![
        crate::EventRecord {
            identity: crate::EventIdentity::default(),
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
            identity: crate::EventIdentity::default(),
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
    use crate::formats::events::events_lance_only_message;
    let err = into_storyline(TestFormat::CanonicalEvent, "[]").unwrap_err();
    assert!(
        err.to_string().contains("Lance-only")
            || err
                .to_string()
                .contains(events_lance_only_message().split(';').next().unwrap())
    );
    let story = crate::StorylineDocument::new("s", "a");
    assert!(from_storyline(TestFormat::CanonicalEvent, &story).is_err());
    assert!(convert(TestFormat::Atif, TestFormat::CanonicalEvent, "{}").is_err());
    assert!(TestFormat::CanonicalEvent.is_lance_only());
}

#[test]
fn export_events_jsonl_debug_roundtrip_via_test_parser() {
    use crate::formats::events::{
        export_events_jsonl, parse_events_jsonl_for_test, EventRecord, EventsDocument,
    };
    use serde_json::json;
    let events = vec![EventRecord {
        identity: crate::EventIdentity::default(),
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
fn parse_openai_msg_envelope() {
    let raw = r#"{
      "session_id": "s1",
      "session_dir": "s1",
      "agent_id": "a1",
      "run_bucket": "2026-07-29",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s1",
        "step_id": 1,
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
    let value = serde_json::from_str(raw).unwrap();
    let stories =
        crate::formats::parse_openai_msg_corpus_value(&value, "session_steps.json").unwrap();
    assert_eq!(stories.len(), 1);
    assert_eq!(stories[0].session_id, "s1");
    assert_eq!(stories[0].turns[0].message, json!("ping"));
}

#[test]
fn detect_format_from_content_and_path() {
    use crate::formats::detect::{detect_format, detect_format_from_path};
    use std::path::Path;
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/session_steps.json")),
        Some(crate::DocumentFormat::OpenaiMsg)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/events.lance")),
        Some(crate::DocumentFormat::CanonicalEvent)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/sess.md")),
        Some(crate::DocumentFormat::AgenticMd)
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/storyline.json")),
        None
    );
    assert_eq!(
        detect_format_from_path(Path::new("/tmp/x/task.actf.json")),
        Some(crate::DocumentFormat::Actf)
    );
    let atif = r#"{"schema_version":"ATIF-v1.7","session_id":"s","agent":{"name":"a","version":"1"},"steps":[]}"#;
    assert_eq!(
        detect_format(None, Some(atif)).unwrap(),
        Some(crate::DocumentFormat::Atif)
    );
    let atif_with_agenticmd_marker = r#"{"schema_version":"ATIF-v1.7","session_id":"s","agent":{"name":"a","version":"1"},"steps":[{"step_id":1,"source":"user","message":"source contains <!-- persisting:block but remains ATIF"}]}"#;
    assert_eq!(
        detect_format(None, Some(atif_with_agenticmd_marker)).unwrap(),
        Some(crate::DocumentFormat::Atif)
    );
    let atif_ndjson = format!("{atif_with_agenticmd_marker}\n{atif}");
    assert_eq!(
        detect_format(None, Some(&atif_ndjson)).unwrap(),
        Some(crate::DocumentFormat::Atif)
    );
    let story = r#"{"session":"s","agent":{"id":"a"},"turns":[]}"#;
    assert_eq!(detect_format(None, Some(story)).unwrap(), None);
    let actf = r#"{"task_id":"t","category":"test","k":1,"correct":false,"attempts_tried":1,"solved_at":null,"attempts":{"1":{"trajectory":{"schema_version":"ACTF_v1.0","steps":[]}}}}"#;
    assert_eq!(
        detect_format(None, Some(actf)).unwrap(),
        Some(crate::DocumentFormat::Actf)
    );
    let response_only =
        r#"[{"session_id":"s","step_id":1,"response":{"role":"assistant","content":"ok"}}]"#;
    assert_eq!(
        detect_format(None, Some(response_only)).unwrap(),
        Some(crate::DocumentFormat::OpenaiMsg)
    );
}

#[test]
fn openai_msg_preserves_user_and_llm_turns() {
    let raw = r#"{
      "session_id": "s1",
      "session_dir": "s1",
      "agent_id": "a1",
      "run_bucket": "2026-07-29",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s1",
        "step_id": 1,
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
    let story = into_storyline(TestFormat::OpenaiMsg, raw).unwrap();
    assert_eq!(story.turns.len(), 2);
    assert_eq!(story.turns[0].source, "user");
    assert_eq!(story.turns[0].message, serde_json::json!("ping"));
    assert_eq!(story.turns[1].source, "agent");
    assert_eq!(story.turns[1].message, serde_json::json!("pong"));
}

#[test]
fn storyline_wire_uses_short_keys() {
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    let out = from_storyline(
        TestFormat::Storyline,
        &into_storyline(TestFormat::Atif, &atif).unwrap(),
    )
    .unwrap();
    assert!(!out.contains(r#""spec""#));
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
    assert!(out.contains(r#""schema_version": "ATIF-v1.7""#));
    assert!(!out.contains(r#""source""#));
    assert!(!out.contains(r#""message""#));
}

#[test]
fn convert_storyline_agenticmd_preserves_dialogue_and_timing() {
    let story = r#"{
      "schema_version": "storyline/v1",
      "session": "sess-md",
      "agent": { "id": "agent-md", "name": "demo" },
      "turns": [
        { "id": 1, "src": "user", "msg": "ask me" },
        { "id": 2, "src": "agent", "msg": "answer", "latency_ms": 42, "ttft_ms": 7, "model": "m1" }
      ]
    }"#;
    let md = convert(TestFormat::Storyline, TestFormat::AgenticMd, story).unwrap();
    assert!(md.contains("format: persisting"));
    assert!(md.contains("ask me"));
    assert!(md.contains("answer"));
    assert!(md.contains("latency_ms"));
    let back = convert(TestFormat::AgenticMd, TestFormat::Storyline, &md).unwrap();
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
fn events_storyline_roundtrip_preserves_call_id_and_seq() {
    use crate::convert::{events_to_storyline, storyline_to_events};
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            identity: crate::EventIdentity::default(),
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
            identity: crate::EventIdentity::default(),
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
    let raw = r#"{
      "session_id": "s-om",
      "session_dir": "s-om",
      "agent_id": "a-om",
      "run_bucket": "b1",
      "source": "dlcapt-proxy",
      "authoritative": "json_file",
      "session_steps": [{
        "id": "step-1",
        "session_id": "s-om",
        "step_id": 1,
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
    let story = convert(TestFormat::OpenaiMsg, TestFormat::Storyline, raw).unwrap();
    let v: serde_json::Value = serde_json::from_str(&story).unwrap();
    assert_eq!(v["session"], "s-om");
    assert_eq!(v["turns"][0]["msg"], "ping");
    assert_eq!(v["turns"][1]["msg"], "pong");

    let back = convert(TestFormat::Storyline, TestFormat::OpenaiMsg, &story).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&back).unwrap();
    let rows = doc["session_steps"].as_array().unwrap();
    assert_eq!(rows[0]["session_id"], "s-om");
    assert!(!rows.is_empty());
}

#[test]
fn events_storyline_roundtrip_preserves_call_dialogue() {
    use crate::convert::{events_to_storyline, storyline_to_events};
    use crate::formats::events::{EventRecord, EventsDocument};
    use serde_json::json;
    let doc = EventsDocument::new(vec![
        EventRecord {
            identity: crate::EventIdentity::default(),
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
            identity: crate::EventIdentity::default(),
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
    let atif = serde_json::to_string_pretty(&sample_traj()).unwrap();
    let same = convert(TestFormat::Atif, TestFormat::Atif, &atif).unwrap();
    assert_eq!(same, atif);

    let story = convert(TestFormat::Atif, TestFormat::Storyline, &atif).unwrap();
    let back = convert(TestFormat::Storyline, TestFormat::Atif, &story).unwrap();
    let traj: crate::atif::AtifTrajectory = serde_json::from_str(&back).unwrap();
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
        schema_version: crate::model::STORYLINE_SCHEMA_VERSION.into(),
        origin: None,
        run_id: None,
        trajectory_id: None,
        attempt_id: None,
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
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
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
    let atif = serde_json::to_string(&sample_traj()).unwrap();
    let md = convert(TestFormat::Atif, TestFormat::AgenticMd, &atif).unwrap();
    assert!(md.contains("What is the price of GOOGL?"));
    assert!(md.contains("I will search."));
    let story = convert(TestFormat::AgenticMd, TestFormat::Storyline, &md).unwrap();
    let v: serde_json::Value = serde_json::from_str(&story).unwrap();
    assert_eq!(v["session"], "sess-1");
    assert!(v["turns"].as_array().unwrap().len() >= 2);
}
