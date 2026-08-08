//! ACTF ⇄ Storyline conversion.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::formats::actf::{
    ActfAssistantContent, ActfAttempt, ActfDocument, ActfMetric, ActfObservation, ActfStep,
    ActfToolCall, ActfTrajectory, ACTF_SCHEMA_VERSION,
};
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn, STORYLINE_SCHEMA_VERSION,
};
use crate::{Error, Result};

const ACTF_PROVENANCE_KEY: &str = "_pchronicle_actf";
const ACTF_STEP_KEY: &str = "_pchronicle_actf_step";
const ACTF_PROVENANCE_VERSION: u64 = 1;

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
    let provenance_count = stories
        .iter()
        .filter(|story| provenance(story).is_some())
        .count();
    if provenance_count == 0 {
        if stories.len() != 1 {
            return Err(Error::Other(
                "synthesizing ACTF without provenance requires one Storyline".into(),
            ));
        }
        return synthesize_actf(&stories[0]);
    }
    if provenance_count != stories.len() {
        return Err(Error::Other(
            "cannot mix ACTF-provenance and unrelated Storylines".into(),
        ));
    }

    let first = provenance(&stories[0]).expect("provenance count checked above");
    validate_provenance(first)?;
    let root_value = first
        .get("root")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Other("ACTF provenance missing root metadata".into()))?
        .clone();
    let mut attempts = Map::new();
    for story in stories {
        story.validate()?;
        let metadata = provenance(story).expect("provenance count checked above");
        validate_provenance(metadata)?;
        if metadata.get("root").and_then(Value::as_object) != Some(&root_value) {
            return Err(Error::Other(
                "ACTF Storylines have conflicting root metadata".into(),
            ));
        }
        let attempt_id = metadata
            .get("attempt_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Other("ACTF provenance missing attempt_id".into()))?;
        let mut attempt = metadata
            .get("attempt")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| Error::Other("ACTF provenance missing attempt metadata".into()))?;
        let mut trajectory = metadata
            .get("trajectory")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| Error::Other("ACTF provenance missing trajectory metadata".into()))?;
        let steps = story
            .turns
            .iter()
            .map(|turn| {
                turn.extra
                    .as_ref()
                    .and_then(|extra| extra.get(ACTF_STEP_KEY))
                    .cloned()
                    .ok_or_else(|| {
                        Error::Other(format!(
                            "Storyline '{}' step {} lacks ACTF lossless step metadata",
                            story.session_id, turn.id
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        trajectory.insert("steps".into(), Value::Array(steps));
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
    root.insert("attempts".into(), Value::Object(attempts));
    let document: ActfDocument = serde_json::from_value(Value::Object(root))?;
    document.validate()?;
    Ok(document)
}

pub fn is_actf_storyline(story: &StorylineDocument) -> bool {
    provenance(story)
        .is_some_and(|metadata| metadata.get("version").and_then(Value::as_u64) == Some(1))
}

fn attempt_to_storyline(
    document: &ActfDocument,
    attempt_id: &str,
    attempt: &ActfAttempt,
    root_metadata: &Value,
    multiple_attempts: bool,
) -> Result<StorylineDocument> {
    let attempt_metadata = attempt_metadata(attempt)?;
    let trajectory_metadata = trajectory_metadata(&attempt.trajectory)?;
    let mut turns = Vec::with_capacity(attempt.trajectory.steps.len());
    for step in &attempt.trajectory.steps {
        let tool_calls = (!step.tools.is_empty()).then(|| {
            step.tools
                .iter()
                .map(|call| StorylineToolCall {
                    tool_call_id: call.id.clone(),
                    function_name: actf_tool_name(call),
                    arguments: actf_tool_arguments(call),
                    duration_ms: if step.tools.len() == 1 {
                        step.metric.env_action_ms.as_f64().map(|value| value as i64)
                    } else {
                        None
                    },
                    extra: Some(json!({"actf_type": call.kind, "actf_extra": call.extra})),
                })
                .collect::<Vec<_>>()
        });
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
            extra: Some(json!({ "_pchronicle_actf_step": step })),
        });
    }

    let session_id = if multiple_attempts {
        format!("{}#attempt-{attempt_id}", document.task_id)
    } else {
        document.task_id.clone()
    };
    Ok(StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
        run_id: Some(document.task_id.clone()),
        session_id,
        agent: StorylineAgent {
            id: "actf-agent".into(),
            name: Some("ACTF Agent".into()),
            version: Some(ACTF_SCHEMA_VERSION.into()),
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
            "_pchronicle_actf": {
                "version": ACTF_PROVENANCE_VERSION,
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
    let tools = turn
        .tool_calls
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|call| ActfToolCall {
            kind: call
                .extra
                .as_ref()
                .and_then(|extra| extra.get("actf_type"))
                .and_then(Value::as_str)
                .unwrap_or("tool_use")
                .to_string(),
            id: call.tool_call_id.clone(),
            extra: Map::from_iter([
                ("name".into(), Value::String(call.function_name.clone())),
                ("input".into(), call.arguments.clone()),
            ]),
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
            let tool_use_id = result
                .get("tool_use_id")
                .or_else(|| result.get("source_call_id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mut extra = result.as_object().cloned().unwrap_or_default();
            extra.remove("type");
            extra.remove("source_call_id");
            extra
                .entry("tool_use_id")
                .or_insert(Value::String(tool_use_id));
            ActfObservation {
                kind: result
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("tool_result")
                    .to_string(),
                extra,
            }
        })
        .collect::<Vec<_>>();
    let metric = turn
        .metrics
        .clone()
        .and_then(|value| serde_json::from_value::<ActfMetric>(value).ok())
        .unwrap_or(ActfMetric {
            prompt_tokens_len: json!(0),
            completion_tokens_len: json!(0),
            llm_infer_ms: turn.latency_ms.map_or(Value::Null, |value| json!(value)),
            env_action_ms: Value::Null,
            stop_reason: Value::Null,
            extra: Map::new(),
        });
    let timestamp = turn
        .timestamp
        .clone()
        .unwrap_or_else(|| "1970-01-01 00:00:00+00:00".into());
    Ok(ActfStep {
        step_id: turn.id,
        assistant_content: ActfAssistantContent {
            content: turn.message.as_str().unwrap_or("").to_string(),
            reasoning_content: turn.reasoning_content.clone().unwrap_or_default(),
            tool_calls: tools.clone(),
            extra: Map::new(),
        },
        metric,
        system_prompt: String::new(),
        user_content: String::new(),
        tools,
        observation: observations,
        started_at: timestamp.clone(),
        finished_at: timestamp,
        extra: Map::new(),
    })
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
    value
        .as_object_mut()
        .expect("ActfDocument serializes as an object")
        .remove("attempts");
    Ok(value)
}

fn attempt_metadata(attempt: &ActfAttempt) -> Result<Value> {
    let mut value = serde_json::to_value(attempt)?;
    value
        .as_object_mut()
        .expect("ActfAttempt serializes as an object")
        .remove("trajectory");
    Ok(value)
}

fn trajectory_metadata(trajectory: &ActfTrajectory) -> Result<Value> {
    let mut value = serde_json::to_value(trajectory)?;
    value
        .as_object_mut()
        .expect("ActfTrajectory serializes as an object")
        .remove("steps");
    Ok(value)
}

fn provenance(story: &StorylineDocument) -> Option<&Map<String, Value>> {
    story.extra.as_ref()?.get(ACTF_PROVENANCE_KEY)?.as_object()
}

fn validate_provenance(metadata: &Map<String, Value>) -> Result<()> {
    if metadata.get("version").and_then(Value::as_u64) != Some(ACTF_PROVENANCE_VERSION) {
        return Err(Error::Other(
            "unsupported or missing ACTF provenance version".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_actf_document;
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

    #[tokio::test]
    async fn actf_lance_import_and_restore_is_lossless() {
        let document = parse_actf_document(FIXTURE).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();

        store.replace_storyline(&story).await.unwrap();
        let restored_story = store
            .get_storyline(&story.session_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(storyline_to_actf(&restored_story).unwrap(), document);
    }
}
