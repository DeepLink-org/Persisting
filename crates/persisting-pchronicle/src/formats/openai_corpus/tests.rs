use super::*;

fn mapped_fields_fixture() -> Value {
    json!({"session_steps": [{
        "dataset_type": "TEST",
        "id": "event-1",
        "session_id": "session-1",
        "step_id": 1,
        "job_id": "job-7",
        "agent_model": "model-3",
        "created_at": 1_785_578_400.25,
        "reward": 0.75,
        "step_reward": -0.25,
        "is_terminal": true,
        "is_truncated": false,
        "is_session_completed": true,
        "is_trainable": false,
        "env_id": "session-1",
        "messages": [{
            "role": "user",
            "content": "inspect",
            "name": null,
            "refusal": null,
            "tool_call_id": null,
            "tool_calls": null
        }],
        "response": {
            "role": "assistant",
            "content": "done",
            "name": null,
            "refusal": null,
            "tool_call_id": null,
            "tool_calls": null
        },
        "meta_json": {
            "source": "fixture",
            "group_id": "group-1",
            "env_state": {
                "session_id": "session-1",
                "requested_model": "model-3",
                "llm_step_index": 1,
                "total_tokens": 3,
                "created_at": "2026-08-01T00:00:00Z",
                "is_session_completed": false,
                "total_latency_ms": 12.75,
                "ttft_ms": 2.5,
                "request_id": "request-1"
            }
        },
        "blob_manifest": [],
        "chosen_response": null,
        "vendor_row": {"kept": true}
    }]})
}

#[test]
fn openai_unknown_fields_use_exact_row_paths() {
    let input = json!({"root_vendor": 1, "session_steps": [{
        "session_id": "s", "step_id": 1,
        "messages": [{
            "role": "user", "content": "hi", "message_vendor": null, "0": true
        }],
        "response": {"role": "assistant", "content": "ok"},
        "row_vendor": [3, 2, 1]
    }]});
    let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
    let fields = &stories[0].unknown_fields.sources["openai-msg"].fields;
    assert_eq!(fields["/root_vendor"], 1);
    assert_eq!(fields["/session_steps/0/row_vendor"], json!([3, 2, 1]));
    assert_eq!(
        fields["/session_steps/0/messages/0/message_vendor"],
        Value::Null
    );
    assert_eq!(
        stories[0].unknown_key_counts["openai-msg"]["/session_steps/*/messages/*/0"],
        1
    );
    assert_eq!(
        crate::formats::unknown_fields::compute_unknown_key_counts(&stories[0].unknown_fields)
            .unwrap(),
        stories[0].unknown_key_counts
    );
    stories[0].validate().unwrap();
}

#[test]
fn openai_step_id_maps_to_storyline_turn_ids() {
    let input = json!([{
        "session_id": "s",
        "step_id": 1,
        "messages": [{"role": "user", "content": "hi"}],
        "response": {"role": "assistant", "content": "ok"}
    }]);

    let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();

    assert_eq!(
        stories[0]
            .turns
            .iter()
            .map(|turn| turn.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(stories[0]
        .unknown_fields
        .sources
        .get("openai-msg")
        .is_none_or(|source| !source.fields.contains_key("/session_steps/0/step_id")));
    let recovered = recover_openai_msg_files(&stories).unwrap();
    assert_eq!(recovered[0].document["session_steps"][0]["step_id"], 1);
    stories[0].validate().unwrap();
}

#[test]
fn openai_maps_status_and_env_fields_into_storyline() {
    let input = mapped_fields_fixture();

    let stories = parse_openai_msg_corpus_value(&input, "source.json").unwrap();
    let story = &stories[0];

    assert_eq!(story.agent.id, "fixture");
    assert_eq!(story.run_id.as_deref(), Some("job-7"));
    assert_eq!(story.agent.model_name.as_deref(), Some("model-3"));
    assert_eq!(
        story.turns[1].metrics.as_ref().unwrap()["is_terminal"],
        true
    );
    assert_eq!(
        story.turns[1].metrics.as_ref().unwrap()["is_session_completed"],
        true
    );
    assert_eq!(story.turns[1].metrics.as_ref().unwrap()["total_tokens"], 3);
    assert_eq!(
        story.turns[1].metrics.as_ref().unwrap()["created_at"],
        "2026-08-01T00:00:00Z"
    );
    assert_eq!(story.turns[1].latency_ms, Some(12));
    assert_eq!(story.turns[1].ttft_ms, Some(2));
    let fields = story
        .unknown_fields
        .sources
        .get("openai-msg")
        .map(|source| &source.fields);
    assert!(fields.is_none_or(|fields| {
        !fields.contains_key("/session_steps/0/meta_json/env_state/created_at")
            && !fields.contains_key("/session_steps/0/meta_json/env_state/is_session_completed")
    }));
}

#[test]
fn openai_only_reports_unmapped_source_fields() {
    let input = mapped_fields_fixture();

    let stories = parse_openai_msg_corpus_value(&input, "source.json").unwrap();
    let fields = &stories[0].unknown_fields.sources["openai-msg"].fields;

    assert_eq!(
        fields.get("/session_steps/0/dataset_type"),
        Some(&json!("TEST"))
    );
    assert_eq!(fields.get("/session_steps/0/id"), Some(&json!("event-1")));
    assert_eq!(
        fields.get("/session_steps/0/vendor_row"),
        Some(&json!({"kept": true}))
    );
    assert_eq!(
        fields.get("/session_steps/0/meta_json/group_id"),
        Some(&json!("group-1"))
    );
    assert_eq!(
        fields.get("/session_steps/0/meta_json/env_state/request_id"),
        Some(&json!("request-1"))
    );

    for mapped in [
        "/session_steps/0/step_id",
        "/session_steps/0/is_terminal",
        "/session_steps/0/is_truncated",
        "/session_steps/0/is_session_completed",
        "/session_steps/0/is_trainable",
        "/session_steps/0/env_id",
        "/session_steps/0/messages/0/role",
        "/session_steps/0/messages/0/content",
        "/session_steps/0/messages/0/name",
        "/session_steps/0/messages/0/refusal",
        "/session_steps/0/messages/0/tool_call_id",
        "/session_steps/0/messages/0/tool_calls",
        "/session_steps/0/response/role",
        "/session_steps/0/response/content",
        "/session_steps/0/response/name",
        "/session_steps/0/response/refusal",
        "/session_steps/0/response/tool_call_id",
        "/session_steps/0/response/tool_calls",
        "/session_steps/0/blob_manifest",
        "/session_steps/0/chosen_response",
    ] {
        assert!(
            !fields.contains_key(mapped),
            "mapped field leaked into unknowns: {mapped}"
        );
    }
}

#[test]
fn openai_string_meta_maps_without_using_extra() {
    let input = json!({"session_steps": [{
        "session_id": "session-1",
        "step_id": 9,
        "env_id": "env-1",
        "job_id": "job-7",
        "agent_model": "model-3",
        "messages": [{"role": "user", "content": "inspect"}],
        "response": {"role": "assistant", "content": "done"},
        "meta_json": "{\"source\":\"fixture\",\"env_state\":\"{\\\"total_tokens\\\":3}\"}",
        "vendor_row": {"kept": true}
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "source.json").unwrap();
    let story = &stories[0];
    let fields = &story.unknown_fields.sources["openai-msg"].fields;
    assert_eq!(fields["/session_steps/0/vendor_row"], json!({"kept": true}));
    assert_eq!(fields["/session_steps/0/env_id"], "env-1");
    assert!(!fields.contains_key("/session_steps/0/meta_json"));
    assert!(story.extra.is_none());
    assert!(story.turns.iter().all(|turn| turn.extra.is_none()));
    assert_eq!(story.agent.id, "fixture");
    assert_eq!(story.agent.model_name.as_deref(), Some("model-3"));
    assert_eq!(story.run_id.as_deref(), Some("job-7"));
    assert_eq!(story.turns[1].metrics.as_ref().unwrap()["total_tokens"], 3);

    let recovered = recover_openai_msg_files(&stories).unwrap();
    let reparsed = parse_openai_msg_corpus_value(&recovered[0].document, "source.json").unwrap();
    assert_eq!(reparsed[0].turns, story.turns);
}

#[test]
fn openai_rejects_invalid_non_null_created_at() {
    let input = json!({"session_steps": [{
        "session_id": "session-1",
        "step_id": 1,
        "created_at": "not-a-timestamp",
        "messages": [{"role": "user", "content": "inspect"}],
        "response": {"role": "assistant", "content": "done"}
    }]});

    let error = parse_openai_msg_corpus_value(&input, "invalid-created-at.json").unwrap_err();
    assert_eq!(error.location(), Some("rows[0].created_at"));
    assert!(error.to_string().contains("timestamp"), "{error}");
}

#[test]
fn openai_refusal_only_response_is_a_known_output() {
    let input = json!({"session_steps": [{
        "session_id": "session-1",
        "step_id": 1,
        "messages": [{"role": "user", "content": "unsafe request"}],
        "response": {
            "role": "assistant",
            "content": null,
            "refusal": "I cannot help with that."
        }
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "refusal.json").unwrap();
    assert_eq!(stories[0].turns[1].message, "I cannot help with that.");
    assert!(stories[0]
        .unknown_fields
        .sources
        .get("openai-msg")
        .is_none_or(|source| {
            !source
                .fields
                .contains_key("/session_steps/0/response/refusal")
        }));
    assert!(stories[0].turns.iter().all(|turn| turn.extra.is_none()));
}

#[test]
fn openai_maps_explicit_reasoning_content() {
    let input = json!({"session_steps": [{
        "session_id": "session-1",
        "step_id": 1,
        "messages": [{"role": "user", "content": "question"}],
        "response": {
            "role": "assistant",
            "content": "answer",
            "reasoning_content": "reasoning"
        }
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "reasoning.json").unwrap();
    assert_eq!(
        stories[0].turns[1].reasoning_content.as_deref(),
        Some("reasoning")
    );
    assert!(stories[0]
        .unknown_fields
        .sources
        .get("openai-msg")
        .is_none_or(|source| {
            !source
                .fields
                .contains_key("/session_steps/0/response/reasoning_content")
        }));
}

#[test]
fn openai_imports_first_row_context_once_and_offsets_step_ids() {
    let input = json!({"session_steps": [
        {
            "session_id": "s",
            "step_id": 1,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "prior question"},
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "first answer"}
            ],
            "response": null
        },
        {
            "session_id": "s",
            "step_id": 2,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "prior question"},
                {"role": "user", "content": "first question"},
                {"role": "assistant", "content": "first answer"},
                {"role": "user", "content": "second question"},
                {"role": "assistant", "content": "second answer"}
            ],
            "response": null
        }
    ]});

    let story = parse_openai_msg_corpus_value(&input, "context.json")
        .unwrap()
        .remove(0);
    assert_eq!(story.turns.len(), 6);
    assert_eq!(
        story.turns.iter().map(|turn| turn.id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
    assert_eq!(story.turns[0].source, "system");
    assert_eq!(story.turns[0].message, "policy");
    assert_eq!(story.turns[1].message, "prior question");
    assert_eq!(story.turns[0].is_copied_context, Some(true));
    assert_eq!(story.turns[1].is_copied_context, Some(true));
    assert_eq!(story.turns[2].message, "first question");
    assert_eq!(story.turns[3].message, "first answer");
    assert_eq!(story.turns[4].message, "second question");
    assert_eq!(story.turns[5].message, "second answer");
}

#[test]
fn openai_logical_roundtrip_preserves_mapped_storyline_fields() {
    let input = json!({"session_steps": [
        {
            "session_id": "s",
            "step_id": 1,
            "agent_model": "model-1",
            "is_terminal": false,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "one"}
            ],
            "response": {"role": "assistant", "content": "first"}
        },
        {
            "session_id": "s",
            "step_id": 2,
            "agent_model": "model-1",
            "is_terminal": true,
            "messages": [
                {"role": "system", "content": "policy"},
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "first"},
                {"role": "user", "content": "two"}
            ],
            "response": {"role": "assistant", "content": "second"}
        }
    ]});

    let first = parse_openai_msg_corpus_value(&input, "logical.json").unwrap();
    let encoded = recover_openai_msg_files(&first).unwrap().remove(0).document;
    let second = parse_openai_msg_corpus_value(&encoded, "logical.json").unwrap();
    assert_eq!(second, first);
    assert_eq!(
        encoded["session_steps"][0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        encoded["session_steps"][1]["messages"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(encoded["session_steps"][1]["step_id"], 2);
}

#[test]
fn openai_embedded_tool_call_exports_as_canonical_structured_call() {
    let input = json!({"session_steps": [{
        "session_id": "session-1",
        "step_id": 1,
        "messages": [
            {"role": "user", "content": "inspect"},
            {
                "role": "assistant",
                "content": "<tool_call>execute\n<parameter=command>pwd</parameter>",
                "tool_calls": null
            }
        ],
        "response": {
            "role": "assistant",
            "content": "",
            "tool_calls": null
        }
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "embedded.json").unwrap();
    assert!(stories[0]
        .unknown_fields
        .sources
        .get("openai-msg")
        .is_none_or(|source| source.fields.is_empty()));
    assert_eq!(
        stories[0]
            .turns
            .last()
            .unwrap()
            .tool_calls
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    assert!(stories[0]
        .turns
        .last()
        .unwrap()
        .tool_calls
        .as_ref()
        .unwrap()
        .iter()
        .all(|call| call.extra.is_none()));

    let recovered = recover_openai_msg_files(&stories).unwrap();
    let response = &recovered[0].document["session_steps"][0]["response"];
    assert_eq!(response["tool_calls"][0]["function"]["name"], "execute");
    assert_eq!(
        response["tool_calls"][0]["function"]["arguments"],
        serde_json::to_string(&json!({"command": "pwd"})).unwrap()
    );
    let reparsed = parse_openai_msg_corpus_value(&recovered[0].document, "embedded.json").unwrap();
    assert_eq!(reparsed, stories);
}

#[test]
fn canonical_export_restores_unmapped_row_fields() {
    let input = json!({"session_steps": [{
        "id": "event-1",
        "session_id": "session-1",
        "step_id": 9,
        "job_id": "job-7",
        "run_bucket": "",
        "group_id": "group-4",
        "env_name": "production",
        "agent_id": "agent-2",
        "agent_model": "model-3",
        "llm_model": "",
        "created_at": 1_785_578_400.25,
        "reward": 0.75,
        "step_reward": -0.25,
        "is_terminal": false,
        "is_truncated": true,
        "is_session_completed": false,
        "is_trainable": false,
        "meta": {"source": "fixture", "nested": {"x": 1}},
        "env_state": {"retry_count": 2, "custom": "kept"},
        "metrics": {"cost": 1.5},
        "messages": [{"role": "user", "content": "inspect"}],
        "response": {"role": "assistant", "content": "done"}
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "source.json").unwrap();
    let recovered = recover_openai_msg_files(&stories).unwrap();
    let row = &recovered[0].document["session_steps"][0];

    for key in [
        "id",
        "job_id",
        "run_bucket",
        "group_id",
        "env_name",
        "agent_id",
        "agent_model",
        "llm_model",
        "created_at",
        "reward",
        "step_reward",
        "is_terminal",
        "is_truncated",
        "is_session_completed",
        "is_trainable",
        "meta",
        "env_state",
        "metrics",
    ] {
        assert_eq!(row[key], input["session_steps"][0][key], "key={key}");
    }
    assert!(row.get("call_id").is_none());
}
#[cfg(feature = "lance-store")]
use crate::store::StorylineLanceStore;

fn multi_session_corpus() -> Value {
    json!([
        {
            "id": "evt-2",
            "session_id": "s-1",
            "step_id": 2,
            "agent_model": "gpt-test",
            "created_at": 1_700_000_001,
            "messages": [
                {"role":"user","content":[{"type":"text","text":"next"}]},
                {"role":"assistant","content":[{"type":"text","text":"world"}]}
            ],
            "response": {"role":"assistant","content":[]},
            "reward": 1.0,
            "unknown": null
        },
        {
            "id": "evt-other",
            "session_id": "s-2",
            "step_id": 1,
            "agent_model": "gpt-test",
            "messages": [
                {"role":"user","content":"tool"},
                {"role":"assistant","content":null,"tool_calls":[{
                    "id":"call-1","type":"function",
                    "function":{"name":"lookup","arguments":"{\"q\":1}"}
                }]}
            ],
            "response": {"role":"assistant","content":""}
        },
        {
            "id": "evt-1",
            "session_id": "s-1",
            "step_id": 1,
            "agent_model": "gpt-test",
            "created_at": 1_700_000_000,
            "messages": [
                {"role":"system","content":"system"},
                {"role":"user","content":"hello"},
                {"role":"assistant","content":"answer"}
            ],
            "response": {"role":"assistant","content":""},
            "meta_json": "{\"source\":\"fixture\",\"env_state\":\"{\\\"created_at\\\":\\\"2026-01-01T00:00:00Z\\\",\\\"total_tokens\\\":3}\"}"
        }
    ])
}

#[test]
fn corpus_roundtrip_emits_canonical_rows_in_session_step_order() {
    let input = multi_session_corpus();
    let stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
    assert_eq!(stories.len(), 2);
    assert_eq!(stories[0].turns.len(), 5);
    assert_eq!(stories[0].turns[0].id, 1);
    assert_eq!(stories[0].turns[1].id, 2);
    assert_eq!(stories[0].turns[0].source, "system");
    assert_eq!(stories[0].turns[0].message, json!("system"));
    assert_eq!(stories[0].turns[1].source, "user");
    assert_eq!(stories[0].turns[1].message, json!("hello"));
    assert_eq!(stories[0].turns[2].source, "agent");
    assert_eq!(stories[0].turns[2].message, json!("answer"));
    assert_eq!(stories[1].turns[1].tool_calls.as_ref().unwrap().len(), 1);

    let recovered = recover_openai_msg_files(&stories).unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
    let rows = recovered[0].document["session_steps"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["session_id"], "s-1");
    assert_eq!(rows[0]["step_id"], 1);
    assert_eq!(rows[1]["session_id"], "s-1");
    assert_eq!(rows[1]["step_id"], 2);
    assert_eq!(rows[2]["session_id"], "s-2");
    assert_eq!(rows[2]["step_id"], 1);
}

#[test]
fn synthesis_rejects_user_turn_without_agent_response() {
    let mut stories =
        parse_openai_msg_corpus_value(&multi_session_corpus(), "corpus.json").unwrap();
    stories[0].turns.truncate(2);

    let error = synthesize_openai_msg_corpus_value(&stories[..1]).unwrap_err();
    assert!(error
        .to_string()
        .contains("OpenAI synthesis requires an agent response after user turn 2"));

    stories[0].turns[1].source = "system".into();
    let error = synthesize_openai_msg_corpus_value(&stories[..1]).unwrap_err();
    assert!(error
        .to_string()
        .contains("OpenAI synthesis cannot represent Storyline turn 2 source 'system'"));
}

#[test]
fn openai_unknown_fields_preserve_values_but_storyline_content_is_authoritative() {
    let input = multi_session_corpus();
    let mut stories = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
    assert!(!serde_json::to_string(&stories)
        .unwrap()
        .contains(&["_pchron", "icle_"].concat()));

    stories[0].turns[1].message = json!("edited user");
    stories[0].turns[2].message = json!("edited assistant");
    let recovered = recover_openai_msg_files(&stories).unwrap();
    let rows = recovered[0].document["session_steps"].as_array().unwrap();
    let first_session_row = rows
        .iter()
        .find(|row| row["session_id"] == "s-1" && row["step_id"] == 1)
        .unwrap();
    assert_eq!(first_session_row["messages"][1]["content"], "edited user");
    assert_eq!(first_session_row["response"]["content"], "edited assistant");
    assert_eq!(rows[0]["unknown"], Value::Null);
    let ids = rows
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    assert_eq!(ids, HashSet::from(["evt-1", "evt-2", "evt-other"]));
}

#[test]
fn canonical_export_restores_message_unknowns_and_tool_results_once() {
    let input = json!({"session_steps": [{
        "id": "",
        "session_id": "s",
        "agent_id": "agent",
        "step_id": 1,
        "messages": [
            {"role": "user", "content": "run", "vendor_message": 7},
            {"role": "tool", "tool_call_id": "call-1", "content": "ok"}
        ],
        "response": {
            "role": "assistant",
            "content": "done",
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {"name": "inspect", "arguments": "{}"}
            }]
        }
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "tool-results.json").unwrap();
    assert_eq!(
        stories[0].turns[1].observation.as_ref().unwrap()["results"][0],
        json!({"source_call_id": "call-1", "content": "ok"})
    );
    let recovered = recover_openai_msg_files(&stories).unwrap();
    let row = &recovered[0].document["session_steps"][0];
    assert_eq!(row["agent_id"], "agent");
    assert_eq!(row["id"], "");
    assert_eq!(row["messages"][0]["vendor_message"], 7);
    assert_eq!(
        row["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
    assert_ne!(row["created_at"], "");
}

#[test]
fn historical_tool_results_attach_to_the_originating_turn() {
    let input = json!({"session_steps": [
        {
            "id": "completion-1",
            "session_id": "s",
            "step_id": 1,
            "messages": [{"role": "user", "content": "run"}],
            "response": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "inspect", "arguments": "{}"}
                }]
            }
        },
        {
            "id": "completion-2",
            "session_id": "s",
            "step_id": 2,
            "messages": [
                {"role": "user", "content": "run"},
                {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {"name": "inspect", "arguments": "{}"}
                    }]
                },
                {"role": "tool", "tool_call_id": "call-1", "content": "ok"}
            ],
            "response": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-2",
                    "type": "function",
                    "function": {"name": "finish", "arguments": "{}"}
                }]
            }
        }
    ]});

    let stories = parse_openai_msg_corpus_value(&input, "history.json").unwrap();
    assert_eq!(
        stories[0].turns[1].observation.as_ref().unwrap()["results"][0],
        json!({"source_call_id": "call-1", "content": "ok"})
    );
    assert!(stories[0].turns[3].observation.is_none());
    let tables = crate::store::split_storyline(&stories[0]).unwrap();
    assert_eq!(tables.tool_calls.len(), 2);

    let recovered = recover_openai_msg_files(&stories).unwrap();
    let messages = recovered[0].document["session_steps"][1]["messages"]
        .as_array()
        .unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .count(),
        1
    );
    assert!(messages
        .iter()
        .any(|message| { message["role"] == "tool" && message["tool_call_id"] == "call-1" }));
}

#[test]
fn canonical_export_preserves_message_and_argument_semantics() {
    let input = json!({"session_steps": [{
        "id": "event-1",
        "session_id": "s",
        "step_id": 1,
        "messages": [
            {"role": "system", "name": "policy", "content": "rules"},
            {
                "role": "tool",
                "name": "lookup",
                "tool_call_id": "call-0",
                "content": {"items": [1]}
            },
            {
                "role": "user",
                "name": "operator",
                "content": [{"type": "text", "text": "go"}]
            }
        ],
        "response": {
            "role": "assistant",
            "name": "worker",
            "content": [{"type": "text", "text": "done"}],
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "inspect",
                    "arguments": " { \"q\" : 1 } "
                }
            }]
        }
    }]});

    let stories = parse_openai_msg_corpus_value(&input, "message-shape.json").unwrap();
    let recovered = recover_openai_msg_files(&stories).unwrap();
    let actual = recovered[0].document.clone();
    let pointer = "/session_steps/0/response/tool_calls/0/function/arguments";
    let actual_arguments: Value =
        serde_json::from_str(actual.pointer(pointer).and_then(Value::as_str).unwrap()).unwrap();
    let expected_arguments: Value =
        serde_json::from_str(input.pointer(pointer).and_then(Value::as_str).unwrap()).unwrap();
    assert_eq!(actual_arguments, expected_arguments);
    let reparsed = parse_openai_msg_corpus_value(&actual, "message-shape.json").unwrap();
    assert_eq!(reparsed[0].turns, stories[0].turns);
    let names = actual["session_steps"][0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message.get("name").and_then(Value::as_str))
        .chain(
            actual["session_steps"][0]["response"]
                .get("name")
                .and_then(Value::as_str),
        )
        .collect::<HashSet<_>>();
    assert_eq!(
        names,
        HashSet::from(["policy", "lookup", "operator", "worker"])
    );
}

#[test]
fn envelope_roundtrip_preserves_root_metadata() {
    let input = json!({
        "session_id": "s-1",
        "custom": null,
        "session_steps": [multi_session_corpus()[0].clone()]
    });
    let stories = parse_openai_msg_corpus_value(&input, "session_steps.json").unwrap();
    let recovered = recover_openai_msg_files(&stories).unwrap();
    assert_eq!(recovered[0].document["custom"], Value::Null);
    assert_eq!(recovered[0].document["session_id"], "s-1");
    assert!(recovered[0].document["session_steps"].is_array());
}

#[test]
fn corpus_preserves_run_group_and_user_agent_turns() {
    let input = json!([{
        "id": "call-1",
        "session_id": "child-session",
        "job_id": "shared-run",
        "step_id": 7,
        "messages": [{"role":"user","content":"question"}],
        "response": {"role":"assistant","content":"answer"},
        "is_session_completed": true
    }]);
    let stories = parse_openai_msg_corpus_value(&input, "gateway.json").unwrap();
    assert_eq!(stories[0].run_id.as_deref(), Some("shared-run"));
    assert_eq!(stories[0].turns.len(), 2);
    assert_eq!(stories[0].turns[0].source, "user");
    assert_eq!(stories[0].turns[1].source, "agent");
    let recovered = recover_openai_msg_files(&stories).unwrap();
    let row = &recovered[0].document["session_steps"][0];
    assert_eq!(row["session_id"], "child-session");
    assert_eq!(row["step_id"], 7);
    assert_eq!(row["messages"][0]["content"], "question");
    assert_eq!(row["response"]["content"], "answer");
}

#[test]
fn semantic_encoder_does_not_silently_synthesize_mixed_provenance() {
    let mut stories =
        parse_openai_msg_corpus_value(&multi_session_corpus(), "corpus.json").unwrap();
    let mut unrelated = stories[0].clone();
    unrelated.session_id = "unrelated".into();
    unrelated.trajectory_id = Some("unrelated".into());
    unrelated.origin = None;
    unrelated.extra = None;
    unrelated.unknown_fields = Default::default();
    unrelated.unknown_key_counts = Default::default();
    stories.push(unrelated);

    let error =
        crate::document::encode_json_storylines(crate::format::DocumentFormat::OpenaiMsg, &stories)
            .unwrap_err();
    assert!(error.to_string().contains("OpenAI"), "{error}");
}

#[test]
fn openai_import_rejects_unsafe_source_paths() {
    let error =
        parse_openai_msg_corpus_value(&multi_session_corpus(), "../escape.json").unwrap_err();
    assert!(error.to_string().contains("unsafe"));
    assert_eq!(error.kind(), crate::input::InputIssueKind::Invalid);
    assert_eq!(error.location(), Some("path"));
    assert!(!error.message().contains("../escape.json"));
}

#[test]
fn embedded_text_tool_calls_are_normalized() {
    let calls = parse_embedded_tool_call(
        Some(&json!([{
            "type":"text",
            "text":"<tool_call>execute_ipython_cell\n<parameter=code>print('ok')</parameter>"
        }])),
        7,
    )
    .unwrap();
    assert_eq!(calls[0].function_name, "execute_ipython_cell");
    assert_eq!(calls[0].tool_call_id, "embedded-7-execute_ipython_cell");
    assert_eq!(calls[0].arguments["code"], "print('ok')");
}

#[cfg(feature = "lance-store")]
#[tokio::test]
async fn openai_canonical_export_survives_lance_roundtrip() {
    let input = multi_session_corpus();
    let expected = parse_openai_msg_corpus_value(&input, "corpus.json").unwrap();
    let canonical = recover_openai_msg_files(&expected).unwrap()[0]
        .document
        .clone();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
    store.replace_storylines(&expected).await.unwrap();

    let session_ids = expected
        .iter()
        .map(|story| story.session_id.clone())
        .collect::<Vec<_>>();
    let restored = store
        .get_storylines_full(&session_ids)
        .await
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect::<Vec<_>>();
    let recovered = recover_openai_msg_files(&restored).unwrap();

    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].relative_path, PathBuf::from("corpus.json"));
    assert_eq!(recovered[0].document, canonical);
}

#[cfg(feature = "lance-store")]
#[tokio::test]
async fn openai_fractional_timestamp_survives_lance_roundtrip() {
    let mut input = multi_session_corpus();
    input[0]["created_at"] = json!(1_700_000_001.123_456_f64);
    let expected = parse_openai_msg_corpus_value(&input, "fractional.json").unwrap();
    let canonical = recover_openai_msg_files(&expected).unwrap()[0]
        .document
        .clone();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
    store.replace_storylines(&expected).await.unwrap();

    let session_ids = expected
        .iter()
        .map(|story| story.session_id.clone())
        .collect::<Vec<_>>();
    let restored = store
        .get_storylines_full(&session_ids)
        .await
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect::<Vec<_>>();

    assert_eq!(
        recover_openai_msg_files(&restored).unwrap()[0].document,
        canonical
    );
}

#[cfg(feature = "lance-store")]
#[tokio::test]
async fn openai_null_source_fields_survive_lance_roundtrip() {
    let input = json!([{
        "id": null,
        "session_id": "s-null",
        "step_id": 1,
        "created_at": null,
        "messages": [{"role": "assistant", "content": "ok"}]
    }]);
    let expected = parse_openai_msg_corpus_value(&input, "nulls.json").unwrap();
    let canonical = recover_openai_msg_files(&expected).unwrap()[0]
        .document
        .clone();
    let temporary = tempfile::tempdir().unwrap();
    let store = StorylineLanceStore::open(temporary.path()).await.unwrap();
    store.replace_storylines(&expected).await.unwrap();

    let document_ids = expected
        .iter()
        .map(|story| story.document_id().to_string())
        .collect::<Vec<_>>();
    let restored = store
        .get_storylines_by_document_ids(&document_ids)
        .await
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect::<Vec<_>>();

    assert_eq!(
        recover_openai_msg_files(&restored).unwrap()[0].document,
        canonical
    );
}
