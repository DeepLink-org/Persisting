use std::path::Path;

use anyhow::{Context, Result};
use persisting_pchronicle::document::{
    DocumentFormat, decode_json_storylines, encode_json_storylines,
};
use persisting_pchronicle::model::{StorylineDocument, StorylineTimestamp};
use serde_json::{Map, Value, json};

mod support;

use support::{LookupStrategy, persist_and_restore};

const JSON_FORMATS: [DocumentFormat; 3] = [
    DocumentFormat::Atif,
    DocumentFormat::Actf,
    DocumentFormat::OpenaiMsg,
];

fn timestamp_nanos(timestamp: &Option<StorylineTimestamp>) -> Option<i64> {
    timestamp.as_ref().map(StorylineTimestamp::timestamp_nanos)
}

struct FormatCase {
    name: &'static str,
    format: DocumentFormat,
    relative_path: &'static str,
    input: Value,
}

fn canonical_semantic_json(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(key, value)| {
                    if (key == "created_at" || key.ends_with("_at"))
                        && let Ok(timestamp) = StorylineTimestamp::from_json(value.clone())
                    {
                        return (key, json!(timestamp.canonical_rfc3339()));
                    }
                    (key, canonical_semantic_json(value))
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(canonical_semantic_json).collect())
        }
        value => value,
    }
}

fn assert_semantic_json_eq(actual: &Value, expected: &Value) {
    assert_eq!(
        canonical_semantic_json(actual.clone()),
        canonical_semantic_json(expected.clone())
    );
}

fn format_cases() -> Vec<FormatCase> {
    vec![
        FormatCase {
            name: "atif",
            format: DocumentFormat::Atif,
            relative_path: "semantic.atif.json",
            input: json!({
                "schema_version": "ATIF-v1.7",
                "trajectory_id": "semantic-atif",
                "session_id": null,
                "agent": {
                    "name": "agent",
                    "version": "1",
                    "model_name": null,
                    "vendor_agent": {"enabled": true}
                },
                "steps": [
                    {
                        "step_id": 1,
                        "source": "user",
                        "message": "inspect",
                        "reasoning_content": null,
                        "vendor_step": {"ordinal": 1},
                        "vendor_null": null
                    },
                    {
                        "step_id": 2,
                        "source": "agent",
                        "message": "done",
                        "tool_calls": [{
                            "tool_call_id": "call-1",
                            "function_name": "inspect",
                            "arguments": {"path": "/tmp"},
                            "result": null,
                            "vendor_call": [1, 2, 3]
                        }],
                        "observation": {
                            "results": [{"source_call_id": "call-1", "content": "ok"}]
                        },
                        "vendor_step": {"ordinal": 2}
                    }
                ],
                "vendor/root": {"tilde~key": 7}
            }),
        },
        FormatCase {
            name: "actf",
            format: DocumentFormat::Actf,
            relative_path: "semantic.actf.json",
            input: json!({
                "task_id": "semantic-actf",
                "category": "regression",
                "k": 1,
                "correct": true,
                "attempts_tried": 1,
                "solved_at": null,
                "attempts": {
                    "1": {
                        "correct": true,
                        "final_answer": null,
                        "ground_truth": "done",
                        "trajectory": {
                            "schema_version": "ACTF_v1.0",
                            "steps": [
                                {
                                    "step_id": 1,
                                    "assistant_content": {
                                        "content": "working",
                                        "reasoning_content": "inspect",
                                        "tool_calls": []
                                    },
                                    "metric": {
                                        "prompt_tokens_len": null,
                                        "completion_tokens_len": null,
                                        "llm_infer_ms": null,
                                        "env_action_ms": null,
                                        "stop_reason": null
                                    },
                                    "system_prompt": "system",
                                    "user_content": "inspect",
                                    "tools": [],
                                    "observation": [],
                                    "started_at": "2026-08-20T00:00:00Z",
                                    "finished_at": "2026-08-20T00:00:01Z",
                                    "vendor_step": {"ordinal": 1},
                                    "vendor_null": null
                                },
                                {
                                    "step_id": 2,
                                    "assistant_content": {
                                        "content": "done",
                                        "reasoning_content": "complete",
                                        "tool_calls": [{
                                            "id": "call-1",
                                            "type": "tool_use",
                                            "name": "inspect",
                                            "input": {"path": "/tmp"},
                                            "vendor_call": true
                                        }]
                                    },
                                    "metric": {
                                        "prompt_tokens_len": 10,
                                        "completion_tokens_len": 4,
                                        "llm_infer_ms": 12,
                                        "env_action_ms": 3,
                                        "stop_reason": "stop"
                                    },
                                    "system_prompt": "system",
                                    "user_content": "",
                                    "tools": [{
                                        "id": "call-1",
                                        "type": "tool_use",
                                        "name": "inspect",
                                        "input": {"path": "/tmp"},
                                        "vendor_call": true
                                    }],
                                    "observation": [{
                                        "id": "call-1",
                                        "type": "tool_result",
                                        "content": "ok"
                                    }],
                                    "started_at": "2026-08-20T00:00:01Z",
                                    "finished_at": "2026-08-20T00:00:02Z",
                                    "vendor_step": {"ordinal": 2}
                                }
                            ],
                            "started_at": "2026-08-20T00:00:00Z",
                            "finished_at": "2026-08-20T00:00:02Z",
                            "vendor_trajectory": "kept"
                        },
                        "status": "completed",
                        "score": null,
                        "error": "",
                        "artifacts": {},
                        "extra": {},
                        "analysis_result": {},
                        "meta": {"suite": "semantic"},
                        "vendor_attempt": ["kept"]
                    }
                },
                "vendor/root": {"tilde~key": 7}
            }),
        },
        FormatCase {
            name: "openai",
            format: DocumentFormat::OpenaiMsg,
            relative_path: "semantic.openai.json",
            input: json!({
                "vendor/root": {"tilde~key": 7},
                "root_null": null,
                "session_steps": [{
                    "session_id": "semantic-openai",
                    "step_id": 1,
                    "created_at": 1787184000,
                    "messages": [{
                        "role": "user",
                        "content": "inspect",
                        "vendor_message": {"ordinal": 1}
                    }],
                    "response": {
                        "role": "assistant",
                        "content": "done",
                        "name": null,
                        "tool_calls": null,
                        "vendor_response": [1, 2]
                    },
                    "agent_model": "model",
                    "reward": null,
                    "vendor_row": {"enabled": true},
                    "vendor_null": null
                }]
            }),
        },
    ]
}

fn assert_unknown_contract(format: DocumentFormat, stories: &[StorylineDocument]) {
    assert_eq!(stories.len(), 1);
    let story = &stories[0];
    match format {
        DocumentFormat::Atif => {
            let source = &story.unknown_fields.sources["atif"];
            assert_eq!(source.fields["/vendor~1root"], json!({"tilde~key": 7}));
            assert_eq!(source.fields["/steps/0/vendor_null"], Value::Null);
            assert_eq!(story.unknown_key_counts["atif"]["/steps/*/vendor_step"], 2);
            assert_eq!(
                source.fields["/steps/1/tool_calls/0/vendor_call"],
                json!([1, 2, 3])
            );
        }
        DocumentFormat::Actf => {
            let source = &story.unknown_fields.sources["actf"];
            assert_eq!(source.fields["/vendor~1root"], json!({"tilde~key": 7}));
            assert_eq!(
                source.fields["/attempts/1/trajectory/steps/0/vendor_null"],
                Value::Null
            );
            assert_eq!(
                story.unknown_key_counts["actf"]["/attempts/1/trajectory/steps/*/vendor_step"],
                2
            );
            assert_eq!(source.fields["/attempts/1/vendor_attempt"], json!(["kept"]));
        }
        DocumentFormat::OpenaiMsg => {
            let source = &story.unknown_fields.sources["openai-msg"];
            assert_eq!(source.fields["/vendor~1root"], json!({"tilde~key": 7}));
            assert_eq!(source.fields["/session_steps/0/vendor_null"], Value::Null);
            assert_eq!(
                story.unknown_key_counts["openai-msg"]["/session_steps/*/vendor_row"],
                1
            );
            assert_eq!(
                source.fields["/session_steps/0/messages/0/vendor_message"],
                json!({"ordinal": 1})
            );
        }
        unsupported => panic!("unexpected JSON format {unsupported}"),
    }
}

fn common_storyline_semantics(stories: &[StorylineDocument]) -> Value {
    Value::Array(
        stories
            .iter()
            .map(|story| {
                json!({
                    "session_id": story.session_id,
                    "turns": story.turns.iter().map(|turn| {
                        let tool_calls = turn.tool_calls.as_deref().unwrap_or_default().iter()
                            .map(|call| json!({
                                "id": call.tool_call_id,
                                "name": call.function_name,
                                "arguments": call.arguments,
                            }))
                            .collect::<Vec<_>>();
                        let observation = turn.observation.as_ref()
                            .and_then(|value| value.get("results"))
                            .and_then(Value::as_array)
                            .map(|results| results.iter().map(|result| json!({
                                "source_call_id": result.get("source_call_id"),
                                "content": result.get("content"),
                            })).collect::<Vec<_>>())
                            .unwrap_or_default();
                        json!({
                            "message": turn.message,
                            "tool_calls": tool_calls,
                            "observation": observation,
                        })
                    }).collect::<Vec<_>>()
                })
            })
            .collect(),
    )
}

fn assert_target_modeled_semantics(
    source: &str,
    target: DocumentFormat,
    stories: &[StorylineDocument],
) {
    assert_eq!(stories.len(), 1);
    let story = &stories[0];
    match (source, target) {
        ("atif", DocumentFormat::Actf) => {
            assert_eq!(story.agent.id, "actf-agent");
            assert_eq!(story.agent.name.as_deref(), Some("ACTF Agent"));
            assert!(story.turns.iter().all(|turn| turn.source == "agent"));
            assert!(
                story
                    .turns
                    .iter()
                    .all(|turn| timestamp_nanos(&turn.timestamp) == Some(0))
            );
            assert!(
                story
                    .turns
                    .iter()
                    .all(|turn| turn.reasoning_content.is_none())
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["prompt_tokens_len"],
                0
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["completion_tokens_len"],
                0
            );
        }
        ("atif", DocumentFormat::OpenaiMsg) => {
            assert_eq!(story.agent.id, "agent");
            assert_eq!(story.agent.name.as_deref(), Some("agent"));
            assert!(story.agent.version.is_none());
            assert_eq!(story.turns[0].source, "user");
            assert_eq!(story.turns[1].source, "agent");
            assert!(story.turns.iter().all(|turn| turn.timestamp.is_none()));
            assert!(story.turns[1].metrics.is_none());
        }
        ("actf", DocumentFormat::Atif) => {
            assert_eq!(story.agent.id, "ACTF Agent");
            assert_eq!(story.agent.name.as_deref(), Some("ACTF Agent"));
            assert_eq!(story.agent.version.as_deref(), Some("unknown"));
            assert_eq!(story.turns[0].reasoning_content.as_deref(), Some("inspect"));
            assert_eq!(
                story.turns[1].reasoning_content.as_deref(),
                Some("complete")
            );
            assert_eq!(
                timestamp_nanos(&story.turns[1].timestamp),
                Some(1_787_184_001_000_000_000)
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["prompt_tokens_len"],
                10
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["completion_tokens_len"],
                4
            );
            assert_eq!(story.turns[1].metrics.as_ref().unwrap()["llm_infer_ms"], 12);
            assert_eq!(story.turns[1].latency_ms, Some(12));
            assert_eq!(
                story.turns[1].tool_calls.as_ref().unwrap()[0].duration_ms,
                Some(3)
            );
        }
        ("actf", DocumentFormat::OpenaiMsg) => {
            assert_eq!(story.agent.id, "actf-agent");
            assert_eq!(story.agent.name.as_deref(), Some("actf-agent"));
            assert_eq!(story.turns[0].reasoning_content.as_deref(), Some("inspect"));
            assert_eq!(
                story.turns[1].reasoning_content.as_deref(),
                Some("complete")
            );
            assert_eq!(
                timestamp_nanos(&story.turns[1].timestamp),
                Some(1_787_184_001_000_000_000)
            );
            assert_eq!(
                story.turns.iter().map(|turn| turn.id).collect::<Vec<_>>(),
                vec![2, 4]
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["total_latency_ms"],
                12
            );
            assert_eq!(story.turns[1].latency_ms, Some(12));
            assert_eq!(
                story.turns[1].tool_calls.as_ref().unwrap()[0].duration_ms,
                None
            );
        }
        ("openai", DocumentFormat::Atif) => {
            assert_eq!(story.agent.id, "model");
            assert_eq!(story.agent.model_name.as_deref(), Some("model"));
            assert_eq!(story.agent.version.as_deref(), Some("unknown"));
            assert!(
                story
                    .turns
                    .iter()
                    .all(|turn| timestamp_nanos(&turn.timestamp) == Some(1_787_184_000_000_000_000))
            );
            assert_eq!(story.turns[1].model_name.as_deref(), Some("model"));
            assert!(story.turns[1].metrics.as_ref().unwrap()["reward"].is_null());
        }
        ("openai", DocumentFormat::Actf) => {
            assert_eq!(story.agent.id, "actf-agent");
            assert_eq!(story.agent.name.as_deref(), Some("ACTF Agent"));
            assert!(story.turns.iter().all(|turn| turn.source == "agent"));
            assert!(
                story
                    .turns
                    .iter()
                    .all(|turn| timestamp_nanos(&turn.timestamp)
                        == Some(1_787_184_000_000_000_000))
            );
            assert_eq!(
                story.turns[1].metrics.as_ref().unwrap()["prompt_tokens_len"],
                0
            );
            assert!(story.turns[1].metrics.as_ref().unwrap()["reward"].is_null());
        }
        _ => panic!("unexpected conversion {source} -> {target}"),
    }
}

fn assert_common_storyline_semantics(actual: &[StorylineDocument], expected: &[StorylineDocument]) {
    assert_semantic_json_eq(
        &common_storyline_semantics(actual),
        &common_storyline_semantics(expected),
    );
}

#[test]
fn semantic_json_treats_null_object_members_as_missing() {
    assert_semantic_json_eq(
        &json!({"root": null, "nested": {"value": null}}),
        &json!({"nested": {}}),
    );
}

#[test]
fn semantic_json_keeps_array_nulls_and_non_null_values_significant() {
    assert_ne!(
        canonical_semantic_json(json!([null])),
        canonical_semantic_json(json!([]))
    );
    assert_ne!(
        canonical_semantic_json(json!({"value": 1})),
        canonical_semantic_json(json!({"value": 2}))
    );
}

#[tokio::test]
async fn direct_formats_survive_lance_semantically() -> Result<()> {
    for case in format_cases() {
        let stories = decode_json_storylines(
            case.format,
            &case.input.to_string(),
            Path::new(case.relative_path),
        )
        .with_context(|| format!("decode {} source", case.name))?;
        assert_unknown_contract(case.format, &stories);

        let expected = encode_json_storylines(case.format, &stories)
            .with_context(|| format!("canonicalize {} source", case.name))?;
        if case.format == DocumentFormat::Atif {
            assert_eq!(expected["session_id"], "semantic-atif");
            assert!(
                expected["steps"][1]["tool_calls"][0]
                    .as_object()
                    .is_some_and(|call| !call.contains_key("result"))
            );
        }

        let restored = persist_and_restore(&stories, LookupStrategy::DocumentIds)
            .await
            .with_context(|| format!("persist {} Storylines", case.name))?;
        assert_unknown_contract(case.format, &restored);
        let actual = encode_json_storylines(case.format, &restored)
            .with_context(|| format!("restore {} source", case.name))?;
        assert_semantic_json_eq(&actual, &expected);
    }
    Ok(())
}

fn bridge_path(format: DocumentFormat) -> &'static str {
    match format {
        DocumentFormat::Atif => "bridge.atif.json",
        DocumentFormat::Actf => "bridge.actf.json",
        DocumentFormat::OpenaiMsg => "bridge.openai.json",
        unsupported => panic!("unexpected JSON format {unsupported}"),
    }
}

#[tokio::test]
async fn every_directed_cross_format_hop_is_semantically_stable_through_lance() -> Result<()> {
    for source in format_cases() {
        let source_stories = decode_json_storylines(
            source.format,
            &source.input.to_string(),
            source.relative_path,
        )
        .with_context(|| format!("decode {} source", source.name))?;
        for bridge in JSON_FORMATS
            .into_iter()
            .filter(|format| *format != source.format)
        {
            let edge = format!("{} -> {bridge}", source.name);
            let bridged = encode_json_storylines(bridge, &source_stories)
                .with_context(|| format!("encode {edge}"))?;
            let bridge_stories =
                decode_json_storylines(bridge, &bridged.to_string(), bridge_path(bridge))
                    .with_context(|| format!("decode {edge}"))?;
            assert_unknown_contract(source.format, &bridge_stories);
            assert_common_storyline_semantics(&bridge_stories, &source_stories);
            assert_target_modeled_semantics(source.name, bridge, &bridge_stories);

            let restored = persist_and_restore(&bridge_stories, LookupStrategy::DocumentIds)
                .await
                .with_context(|| format!("persist {edge}"))?;
            assert_common_storyline_semantics(&restored, &bridge_stories);
            assert_unknown_contract(source.format, &restored);
            assert_target_modeled_semantics(source.name, bridge, &restored);
            let actual = encode_json_storylines(bridge, &restored)
                .with_context(|| format!("restore {edge} bridge"))?;
            assert_semantic_json_eq(&actual, &bridged);
        }
    }
    Ok(())
}
