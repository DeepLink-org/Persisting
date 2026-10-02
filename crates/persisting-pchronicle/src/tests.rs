use serde_json::json;

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestFormat {
    Storyline,
    AgenticMd,
    OpenaiMsg,
    Atif,
}

fn into_storyline(format: TestFormat, input: &str) -> crate::Result<crate::StorylineDocument> {
    match format {
        TestFormat::Storyline => crate::formats::storyline::parse_storyline_document(input),
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
        return Ok(input.to_string());
    }
    from_storyline(to, &into_storyline(from, input)?)
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
    assert_eq!(back.steps[0].source, "user");
    assert_eq!(back.steps[1].source, "agent");
    assert_eq!(back.steps[1].tool_calls.as_ref().unwrap().len(), 2);
    assert_eq!(
        back.steps[1].tool_calls.as_ref().unwrap()[0].function_name,
        "financial_search"
    );
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
    assert_eq!(v["turns"].as_array().unwrap().len(), 2);
    assert_eq!(v["turns"][0]["src"], "user");
    assert_eq!(v["turns"][0]["msg"], "ping");
    assert_eq!(v["turns"][1]["src"], "agent");
    assert_eq!(v["turns"][1]["msg"], "pong");

    let back = convert(TestFormat::Storyline, TestFormat::OpenaiMsg, &story).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&back).unwrap();
    let rows = doc["session_steps"].as_array().unwrap();
    assert_eq!(rows[0]["session_id"], "s-om");
    assert!(!rows.is_empty());
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

#[cfg(feature = "proptest")]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    fn non_lance_format() -> impl Strategy<Value = TestFormat> {
        prop::sample::select(vec![
            TestFormat::Storyline,
            TestFormat::AgenticMd,
            TestFormat::OpenaiMsg,
            TestFormat::Atif,
        ])
    }

    proptest! {
        #[test]
        fn same_format_conversion_is_an_identity_for_non_lance_formats(
            format in non_lance_format(),
            input in proptest::string::string_regex("[A-Za-z0-9 {}\\\"._:-]{0,128}").unwrap(),
        ) {
            prop_assert_eq!(convert(format, format, &input).unwrap(), input);
        }

    }
}
