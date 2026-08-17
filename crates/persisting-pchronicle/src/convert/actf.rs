//! ACTF ⇄ Storyline conversion.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::formats::actf::{
    ActfAttempt, ActfDocument, ActfObservation, ActfStep, ActfToolCall, ActfTrajectory,
    ACTF_SCHEMA_VERSION,
};
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
use crate::{Error, Result};

const ACTF_EXTENSION_KEY: &str = "persisting.dev/actf/v1";

pub fn actf_to_storyline(document: &ActfDocument) -> Result<StorylineDocument> {
    let mut stories = actf_to_storylines(document)?;
    if stories.len() != 1 {
        return Err(Error::Other(format!(
            "ACTF document contains {} attempts; use actf_to_storylines",
            stories.len()
        )));
    }
    Ok(stories.remove(0))
}

pub fn actf_to_storylines(document: &ActfDocument) -> Result<Vec<StorylineDocument>> {
    document.validate()?;
    let root_metadata = root_metadata(document)?;
    let multiple_attempts = document.attempts.len() > 1;
    document
        .attempts
        .iter()
        .map(|(attempt_id, attempt)| {
            attempt_to_storyline(
                document,
                attempt_id,
                attempt,
                &root_metadata,
                multiple_attempts,
            )
        })
        .collect()
}

pub fn storyline_to_actf(story: &StorylineDocument) -> Result<ActfDocument> {
    storylines_to_actf(std::slice::from_ref(story))
}

pub fn storylines_to_actf(stories: &[StorylineDocument]) -> Result<ActfDocument> {
    if stories.is_empty() {
        return Err(Error::Other(
            "ACTF conversion requires at least one Storyline".into(),
        ));
    }
    let residual_count = stories
        .iter()
        .filter(|story| residual(story).is_some())
        .count();
    if residual_count == 0 {
        if stories.len() != 1 {
            return Err(Error::Other(
                "synthesizing ACTF without residual metadata requires one Storyline".into(),
            ));
        }
        return synthesize_actf(&stories[0]);
    }
    if residual_count != stories.len() {
        return Err(Error::Other(
            "cannot mix ACTF residual and unrelated Storylines".into(),
        ));
    }

    let first = residual(&stories[0])
        .ok_or_else(|| Error::Other("ACTF residual disappeared during conversion".into()))?;
    let root_value = first
        .get("root")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Other("ACTF residual missing root metadata".into()))?
        .clone();
    let mut attempts = Map::new();
    for story in stories {
        story.validate()?;
        let metadata = residual(story)
            .ok_or_else(|| Error::Other("ACTF residual disappeared during conversion".into()))?;
        if metadata.get("root").and_then(Value::as_object) != Some(&root_value) {
            return Err(Error::Other(
                "ACTF Storylines have conflicting root residual".into(),
            ));
        }
        let attempt_id = metadata
            .get("attempt_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Other("ACTF residual missing attempt_id".into()))?;
        let mut attempt = metadata
            .get("attempt")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| Error::Other("ACTF residual missing attempt metadata".into()))?;
        let mut trajectory = metadata
            .get("trajectory")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| Error::Other("ACTF residual missing trajectory metadata".into()))?;
        let steps = story
            .turns
            .iter()
            .map(storyline_step_value)
            .collect::<Result<Vec<_>>>()?;
        trajectory.insert("steps".into(), Value::Array(steps));
        let metrics = story.final_metrics.as_ref().and_then(Value::as_object);
        attempt.insert(
            "correct".into(),
            metrics
                .and_then(|value| value.get("correct"))
                .cloned()
                .unwrap_or(Value::Bool(false)),
        );
        attempt.insert(
            "score".into(),
            metrics
                .and_then(|value| value.get("score"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        attempt.insert(
            "status".into(),
            metrics
                .and_then(|value| value.get("status"))
                .cloned()
                .unwrap_or_else(|| Value::String("completed".into())),
        );
        attempt.insert("trajectory".into(), Value::Object(trajectory));
        if attempts
            .insert(attempt_id.to_string(), Value::Object(attempt))
            .is_some()
        {
            return Err(Error::Other(format!(
                "duplicate ACTF attempt id '{attempt_id}'"
            )));
        }
    }

    let mut root = root_value;
    root.insert(
        "task_id".into(),
        Value::String(
            stories[0]
                .run_id
                .clone()
                .unwrap_or_else(|| stories[0].session_id.clone()),
        ),
    );
    root.insert(
        "correct".into(),
        stories[0]
            .final_metrics
            .as_ref()
            .and_then(|value| value.get("task_correct"))
            .cloned()
            .unwrap_or(Value::Bool(false)),
    );
    root.insert("attempts".into(), Value::Object(attempts));
    let document: ActfDocument = serde_json::from_value(Value::Object(root))?;
    document.validate()?;
    Ok(document)
}

pub fn is_actf_storyline(story: &StorylineDocument) -> bool {
    residual(story).is_some()
}

fn attempt_to_storyline(
    document: &ActfDocument,
    attempt_id: &str,
    attempt: &ActfAttempt,
    root_metadata: &Value,
    multiple_attempts: bool,
) -> Result<StorylineDocument> {
    let attempt_metadata = attempt_residual(attempt)?;
    let trajectory_metadata = trajectory_residual(&attempt.trajectory)?;
    let mut turns = Vec::with_capacity(attempt.trajectory.steps.len());
    for step in &attempt.trajectory.steps {
        let tool_calls = (!step.tools.is_empty())
            .then(|| {
                step.tools
                    .iter()
                    .map(|call| {
                        Ok(StorylineToolCall {
                            tool_call_id: call.id.clone(),
                            function_name: actf_tool_name(call),
                            arguments: actf_tool_arguments(call),
                            result: Default::default(),
                            duration_ms: if step.tools.len() == 1 {
                                step.metric.env_action_ms.as_f64().map(|value| value as i64)
                            } else {
                                None
                            },
                            extra: Some(json!({
                                ACTF_EXTENSION_KEY: tool_residual(call)?,
                            })),
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let observation = (!step.observation.is_empty()).then(|| {
            let results = step
                .observation
                .iter()
                .map(|observation| {
                    let mut value = serde_json::to_value(observation)
                        .unwrap_or_else(|_| Value::Object(Map::new()));
                    if let Some(object) = value.as_object_mut() {
                        if let Some(source_call_id) = actf_observation_call_id(observation) {
                            object.insert(
                                "source_call_id".into(),
                                Value::String(source_call_id.to_string()),
                            );
                        }
                    }
                    value
                })
                .collect::<Vec<_>>();
            json!({"results": results})
        });
        turns.push(StorylineTurn {
            id: step.step_id,
            kind: tool_calls.as_ref().map(|_| "autonomous".into()),
            timestamp: Some(step.started_at.clone()),
            source: "agent".into(),
            message: Value::String(step.assistant_content.content.clone()),
            reasoning_content: (!step.assistant_content.reasoning_content.is_empty())
                .then(|| step.assistant_content.reasoning_content.clone()),
            reasoning_effort: None,
            tool_calls,
            observation,
            metrics: Some(serde_json::to_value(&step.metric)?),
            model_name: None,
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms: step.metric.llm_infer_ms.as_f64().map(|value| value as i64),
            ttft_ms: None,
            extra: Some(json!({
                ACTF_EXTENSION_KEY: step_residual(step)?,
            })),
        });
    }

    let session_id = if multiple_attempts {
        format!("{}#attempt-{attempt_id}", document.task_id)
    } else {
        document.task_id.clone()
    };
    Ok(StorylineDocument {
        schema_version: None,
        run_id: Some(document.task_id.clone()),
        attempt_id: None,
        session_id,
        agent: StorylineAgent {
            id: "actf-agent".into(),
            name: Some("ACTF Agent".into()),
            version: None,
            model_name: None,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        final_metrics: Some(json!({
            "correct": attempt.correct,
            "score": attempt.score,
            "status": attempt.status,
            "task_correct": document.correct,
        })),
        continued_trajectory_ref: None,
        extra: Some(json!({
            ACTF_EXTENSION_KEY: {
                "root": root_metadata,
                "attempt_id": attempt_id,
                "attempt": attempt_metadata,
                "trajectory": trajectory_metadata,
            }
        })),
        turns,
    })
}

fn synthesize_actf(story: &StorylineDocument) -> Result<ActfDocument> {
    story.validate()?;
    let epoch = "1970-01-01 00:00:00+00:00".to_string();
    let started_at = story
        .turns
        .first()
        .and_then(|turn| turn.timestamp.clone())
        .unwrap_or_else(|| epoch.clone());
    let finished_at = story
        .turns
        .last()
        .and_then(|turn| turn.timestamp.clone())
        .unwrap_or_else(|| started_at.clone());
    let steps = story
        .turns
        .iter()
        .map(synthesize_step)
        .collect::<Result<Vec<_>>>()?;
    let correct = story
        .final_metrics
        .as_ref()
        .and_then(|metrics| metrics.get("correct"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let score = story
        .final_metrics
        .as_ref()
        .and_then(|metrics| metrics.get("score"))
        .cloned()
        .unwrap_or(Value::Null);
    let status = story
        .final_metrics
        .as_ref()
        .and_then(|metrics| metrics.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();
    let attempt = ActfAttempt {
        correct,
        final_answer: story
            .turns
            .last()
            .map(|turn| turn.message.clone())
            .unwrap_or(Value::Null),
        ground_truth: String::new(),
        trajectory: ActfTrajectory {
            schema_version: ACTF_SCHEMA_VERSION.into(),
            steps,
            started_at,
            finished_at: finished_at.clone(),
            extra: Map::new(),
        },
        status,
        score,
        error: String::new(),
        artifacts: json!({}),
        extra: json!({}),
        analysis_result: json!({}),
        meta: json!({}),
        extensions: Map::new(),
    };
    let mut attempts = BTreeMap::new();
    attempts.insert("1".into(), attempt);
    let document = ActfDocument {
        task_id: story
            .run_id
            .clone()
            .unwrap_or_else(|| story.session_id.clone()),
        category: "unknown".into(),
        k: 1,
        correct,
        attempts_tried: 1,
        solved_at: if correct {
            Value::String(finished_at)
        } else {
            Value::Null
        },
        attempts,
        extra: Map::new(),
    };
    document.validate()?;
    Ok(document)
}

fn synthesize_step(turn: &StorylineTurn) -> Result<ActfStep> {
    serde_json::from_value(storyline_step_value(turn)?)
        .map_err(|error| Error::Other(format!("build ACTF step {}: {error}", turn.id)))
}

fn storyline_step_value(turn: &StorylineTurn) -> Result<Value> {
    let tools = turn
        .tool_calls
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|call| {
            let metadata = call
                .extra
                .as_ref()
                .and_then(|extra| extra.get(ACTF_EXTENSION_KEY))
                .and_then(Value::as_object);
            let mut tool = metadata
                .and_then(|value| value.get("residual"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let kind = metadata
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("tool_use");
            tool.insert("type".into(), Value::String(kind.into()));
            tool.insert("id".into(), Value::String(call.tool_call_id.clone()));
            if metadata
                .and_then(|value| value.get("name_present"))
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                tool.insert("name".into(), Value::String(call.function_name.clone()));
            }
            match metadata
                .and_then(|value| value.get("arguments_key"))
                .and_then(Value::as_str)
                .unwrap_or("input")
            {
                "command" => {
                    let command = call
                        .arguments
                        .get("command")
                        .cloned()
                        .unwrap_or_else(|| call.arguments.clone());
                    tool.insert("command".into(), command);
                }
                "none" => {}
                _ => {
                    tool.insert("input".into(), call.arguments.clone());
                }
            }
            Value::Object(tool)
        })
        .collect::<Vec<_>>();
    let observations = turn
        .observation
        .as_ref()
        .and_then(|observation| observation.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|result| {
            let mut extra = result.as_object().cloned().unwrap_or_default();
            extra.remove("source_call_id");
            Value::Object(extra)
        })
        .collect::<Vec<_>>();
    let metric = turn.metrics.clone().unwrap_or_else(|| {
        json!({
            "prompt_tokens_len": 0,
            "completion_tokens_len": 0,
            "llm_infer_ms": turn.latency_ms.map_or(Value::Null, |value| json!(value)),
            "env_action_ms": Value::Null,
            "stop_reason": Value::Null,
        })
    });
    let timestamp = turn
        .timestamp
        .clone()
        .unwrap_or_else(|| "1970-01-01 00:00:00+00:00".into());
    let metadata = turn
        .extra
        .as_ref()
        .and_then(|extra| extra.get(ACTF_EXTENSION_KEY))
        .and_then(Value::as_object);
    let mut assistant = metadata
        .and_then(|value| value.get("assistant_content"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    assistant.insert(
        "content".into(),
        Value::String(turn.message.as_str().unwrap_or("").to_string()),
    );
    assistant.insert(
        "reasoning_content".into(),
        Value::String(turn.reasoning_content.clone().unwrap_or_default()),
    );
    assistant.insert("tool_calls".into(), Value::Array(tools.clone()));

    let mut step = Map::new();
    step.insert("step_id".into(), json!(turn.id));
    step.insert("assistant_content".into(), Value::Object(assistant));
    step.insert("metric".into(), metric);
    step.insert("tools".into(), Value::Array(tools));
    step.insert("observation".into(), Value::Array(observations));
    step.insert("started_at".into(), Value::String(timestamp));
    if let Some(residual) = metadata
        .and_then(|value| value.get("step"))
        .and_then(Value::as_object)
    {
        merge_residual(&mut step, residual, "step");
    }
    Ok(Value::Object(step))
}

fn actf_tool_name(call: &ActfToolCall) -> String {
    call.extra
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(&call.kind)
        .to_string()
}

fn actf_tool_arguments(call: &ActfToolCall) -> Value {
    if let Some(input) = call.extra.get("input") {
        return input.clone();
    }
    if let Some(command) = call.extra.get("command") {
        return json!({ "command": command });
    }
    Value::Object(call.extra.clone())
}

fn actf_observation_call_id(observation: &ActfObservation) -> Option<&str> {
    observation
        .extra
        .get("tool_use_id")
        .or_else(|| observation.extra.get("id"))
        .and_then(Value::as_str)
}

fn root_metadata(document: &ActfDocument) -> Result<Value> {
    let mut value = serde_json::to_value(document)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::Other("serialized ACTF document must be an object".into()))?;
    for key in ["task_id", "correct", "attempts"] {
        object.remove(key);
    }
    Ok(value)
}

fn attempt_residual(attempt: &ActfAttempt) -> Result<Value> {
    let mut value = serde_json::to_value(attempt)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::Other("serialized ACTF attempt must be an object".into()))?;
    for key in ["correct", "score", "status", "trajectory"] {
        object.remove(key);
    }
    Ok(value)
}

fn trajectory_residual(trajectory: &ActfTrajectory) -> Result<Value> {
    let mut value = serde_json::to_value(trajectory)?;
    value
        .as_object_mut()
        .ok_or_else(|| Error::Other("serialized ACTF trajectory must be an object".into()))?
        .remove("steps");
    Ok(value)
}

fn step_residual(step: &ActfStep) -> Result<Value> {
    let mut value = serde_json::to_value(step)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| Error::Other("serialized ACTF step must be an object".into()))?;
    let mut assistant = object
        .remove("assistant_content")
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| {
            Error::Other("serialized ACTF assistant_content must be an object".into())
        })?;
    for key in ["content", "reasoning_content", "tool_calls"] {
        assistant.remove(key);
    }
    for key in ["step_id", "metric", "tools", "observation", "started_at"] {
        object.remove(key);
    }
    let mut residual = Map::new();
    residual.insert("step".into(), Value::Object(object.clone()));
    residual.insert("assistant_content".into(), Value::Object(assistant));
    Ok(Value::Object(residual))
}

fn tool_residual(call: &ActfToolCall) -> Result<Value> {
    let name_present = call.extra.contains_key("name");
    let arguments_key = if call.extra.contains_key("input") {
        "input"
    } else if call.extra.contains_key("command") {
        "command"
    } else {
        "none"
    };
    let mut residual = call.extra.clone();
    residual.remove("name");
    residual.remove("input");
    residual.remove("command");
    Ok(json!({
        "kind": call.kind,
        "name_present": name_present,
        "arguments_key": arguments_key,
        "residual": residual,
    }))
}

fn merge_residual(target: &mut Map<String, Value>, residual: &Map<String, Value>, scope: &str) {
    for (key, value) in residual {
        if target.contains_key(key) {
            tracing::warn!(
                source_format = "actf",
                source_key = %key,
                target_key = %key,
                scope,
                "ACTF residual conflicts with an authoritative Storyline field"
            );
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}

fn residual(story: &StorylineDocument) -> Option<&Map<String, Value>> {
    story.extra.as_ref()?.get(ACTF_EXTENSION_KEY)?.as_object()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_actf_document;
    #[cfg(feature = "lance-store")]
    use crate::StorylineLanceStore;

    const FIXTURE: &str = r#"{
      "task_id":"task-1","category":"software-engineering","k":1,
      "correct":false,"attempts_tried":1,"solved_at":null,
      "attempts":{"1":{"correct":false,"final_answer":null,"ground_truth":"expected",
      "trajectory":{"schema_version":"ACTF_v1.0","steps":[{
        "step_id":1,
        "assistant_content":{"content":"done","reasoning_content":"think","tool_calls":[{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"pwd"}}]},
        "metric":{"prompt_tokens_len":1,"completion_tokens_len":2,"llm_infer_ms":3.5,"env_action_ms":4.5,"stop_reason":null},
        "system_prompt":"sys","user_content":"task",
        "tools":[{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"pwd"}}],
        "observation":[{"tool_use_id":"c1","type":"tool_result","content":"/app","is_error":false}],
        "started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"
      }],"started_at":"2026-01-01 00:00:00+00:00","finished_at":"2026-01-01 00:00:01+00:00"},
      "status":"completed","score":null,"error":"","artifacts":{},"extra":{},"analysis_result":{},"meta":{}}}
    }"#;

    #[test]
    fn actf_storyline_roundtrip_is_lossless() {
        let document = parse_actf_document(FIXTURE).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        assert_eq!(story.turns.len(), 1);
        assert_eq!(story.turns[0].tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(
            story.turns[0].observation.as_ref().unwrap()["results"][0]["content"],
            "/app"
        );
        assert_eq!(storyline_to_actf(&story).unwrap(), document);
    }

    #[test]
    fn actf_residual_preserves_unknowns_but_storyline_fields_are_authoritative() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["root_unknown"] = Value::Null;
        value["attempts"]["1"]["attempt_unknown"] = json!([3, 2, 1]);
        value["attempts"]["1"]["trajectory"]["trajectory_unknown"] = json!({"x": 1});
        value["attempts"]["1"]["trajectory"]["steps"][0]["step_unknown"] = Value::Null;
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]
            ["assistant_unknown"] = json!("kept");
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"][0]["tool_unknown"] = Value::Null;
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"][0]
            ["tool_unknown"] = Value::Null;
        let document: ActfDocument = serde_json::from_value(value).unwrap();

        let mut story = actf_to_storyline(&document).unwrap();
        assert!(!serde_json::to_string(&story)
            .unwrap()
            .contains("_pchronicle_"));
        story.turns[0].message = json!("changed by Storyline");
        story.turns[0].reasoning_content = Some("new reasoning".into());

        let recovered = storyline_to_actf(&story).unwrap();
        let recovered = serde_json::to_value(recovered).unwrap();
        let step = &recovered["attempts"]["1"]["trajectory"]["steps"][0];
        assert_eq!(step["assistant_content"]["content"], "changed by Storyline");
        assert_eq!(
            step["assistant_content"]["reasoning_content"],
            "new reasoning"
        );
        assert_eq!(recovered["root_unknown"], Value::Null);
        assert_eq!(
            recovered["attempts"]["1"]["attempt_unknown"],
            json!([3, 2, 1])
        );
        assert_eq!(
            recovered["attempts"]["1"]["trajectory"]["trajectory_unknown"],
            json!({"x": 1})
        );
        assert_eq!(step["step_unknown"], Value::Null);
        assert_eq!(step["assistant_content"]["assistant_unknown"], "kept");
        assert_eq!(step["tools"][0]["tool_unknown"], Value::Null);
    }

    #[test]
    fn multiple_attempts_roundtrip_as_multiple_storylines() {
        let mut document = parse_actf_document(FIXTURE).unwrap();
        document.k = 2;
        document.attempts_tried = 2;
        let mut second = document.attempts["1"].clone();
        second.trajectory.steps[0].assistant_content.content = "second attempt".into();
        document.attempts.insert("2".into(), second);

        let stories = actf_to_storylines(&document).unwrap();
        assert_eq!(stories.len(), 2);
        assert_eq!(stories[0].session_id, "task-1#attempt-1");
        assert_eq!(stories[1].session_id, "task-1#attempt-2");
        assert_eq!(storylines_to_actf(&stories).unwrap(), document);
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn actf_lance_import_and_restore_is_lossless() {
        let document = parse_actf_document(FIXTURE).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();

        store.replace_storyline(&story).await.unwrap();
        let restored_story = store
            .get_storyline_full(&story.session_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(storyline_to_actf(&restored_story).unwrap(), document);
    }
}
