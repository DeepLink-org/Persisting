//! ACTF ⇄ Storyline conversion.

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde_json::{json, Map, Value};

use crate::format::DocumentFormat;
use crate::formats::actf::{
    ActfAttempt, ActfDocument, ActfObservation, ActfStep, ActfToolCall, ActfTrajectory,
    ACTF_SCHEMA_VERSION,
};
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineOrigin, StorylineToolCall, StorylineTurn,
    STORYLINE_SCHEMA_VERSION,
};
use crate::formats::timestamp::StorylineTimestamp;
use crate::formats::unknown_fields::{
    decode_json_pointer, normalize_actf_pointer, restore_json_pointer,
    validate_unknown_fields_with, write_foreign_unknown_fields_envelope, CarrierBinding,
    PointerWrite, UnknownFieldLimits,
};
use crate::Result;

fn actf_tool_to_storyline(call: &ActfToolCall, duration_ms: Option<i64>) -> StorylineToolCall {
    StorylineToolCall {
        tool_call_id: call.id.clone(),
        function_name: actf_tool_name(call),
        arguments: actf_tool_arguments(call),
        result: call.extra.get("aggregated_output").cloned(),
        duration_ms,
        extra: None,
    }
}

fn actf_observation_to_storyline(observation: &ActfObservation) -> Value {
    let mut result =
        serde_json::to_value(observation).unwrap_or_else(|_| Value::Object(Map::new()));
    if let Some(object) = result.as_object_mut() {
        if let Some(source_call_id) = actf_observation_call_id(observation) {
            object.insert(
                "source_call_id".into(),
                Value::String(source_call_id.to_string()),
            );
        }
        if let Some(content) = observation
            .extra
            .get("aggregated_output")
            .or_else(|| observation.extra.get("content"))
        {
            object.insert("content".into(), content.clone());
        }
    }
    result
}

pub(crate) fn actf_to_storylines(document: &ActfDocument) -> Result<Vec<StorylineDocument>> {
    document.validate()?;
    let multiple_attempts = document.attempts.len() > 1;
    let mut stories = document
        .attempts
        .iter()
        .map(|(attempt_id, attempt)| {
            attempt_to_storyline(document, attempt_id, attempt, multiple_attempts)
        })
        .collect::<Result<Vec<_>>>()?;
    for ((attempt_id, attempt), story) in document.attempts.iter().zip(stories.iter_mut()) {
        capture_actf_unknowns(document, attempt_id, attempt, story)?;
        story.unknown_key_counts = validate_unknown_fields_with(
            &story.unknown_fields,
            UnknownFieldLimits::default(),
            normalize_actf_pointer,
        )?;
    }
    Ok(stories)
}

pub(crate) fn storylines_to_actf(stories: &[StorylineDocument]) -> Result<ActfDocument> {
    storylines_to_actf_pointer(stories)
}

fn attempt_to_storyline(
    document: &ActfDocument,
    attempt_id: &str,
    attempt: &ActfAttempt,
    multiple_attempts: bool,
) -> Result<StorylineDocument> {
    let mut turns = Vec::with_capacity(attempt.trajectory.steps.len());
    for step in &attempt.trajectory.steps {
        let tool_calls = (!step.tools.is_empty())
            .then(|| {
                step.tools
                    .iter()
                    .map(|call| {
                        Ok(actf_tool_to_storyline(
                            call,
                            if step.tools.len() == 1 {
                                step.metric.env_action_ms.as_f64().map(|value| value as i64)
                            } else {
                                None
                            },
                        ))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let observation = (!step.observation.is_empty()).then(|| {
            let results = step
                .observation
                .iter()
                .map(actf_observation_to_storyline)
                .collect::<Vec<_>>();
            json!({"results": results})
        });
        turns.push(StorylineTurn {
            id: step.step_id,
            kind: tool_calls.as_ref().map(|_| "autonomous".into()),
            timestamp: Some(StorylineTimestamp::from_rfc3339(&step.started_at)?),
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
            extra: None,
        });
    }

    let session_id = if multiple_attempts {
        format!("{}#attempt-{attempt_id}", document.task_id)
    } else {
        document.task_id.clone()
    };
    Ok(StorylineDocument {
        schema_version: STORYLINE_SCHEMA_VERSION.into(),
        origin: Some(StorylineOrigin {
            format: DocumentFormat::Actf.as_str().into(),
            schema_version: Some(ACTF_SCHEMA_VERSION.into()),
            document_id: None,
        }),
        run_id: Some(document.task_id.clone()),
        trajectory_id: None,
        attempt_id: Some(attempt_id.to_string()),
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
            "analysis_result": attempt.analysis_result,
        })),
        continued_trajectory_ref: None,
        extra: None,
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns,
    })
}

fn capture_actf_unknowns(
    document: &ActfDocument,
    attempt_id: &str,
    attempt: &ActfAttempt,
    story: &mut StorylineDocument,
) -> crate::InputResult<()> {
    let source_id = &document.task_id;
    let mut root = serde_json::to_value(document)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    let root = root
        .as_object_mut()
        .ok_or_else(|| crate::InputIssue::invalid("serialized ACTF document must be an object"))?;
    for key in ["task_id", "correct", "attempts"] {
        root.remove(key);
    }
    insert_actf_map(story, source_id, "", root)?;

    let attempt_prefix = pointer_join("/attempts", attempt_id);
    let mut attempt_value = serde_json::to_value(attempt)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    let attempt_map = attempt_value
        .as_object_mut()
        .ok_or_else(|| crate::InputIssue::invalid("serialized ACTF attempt must be an object"))?;
    for key in [
        "correct",
        "trajectory",
        "status",
        "score",
        "analysis_result",
    ] {
        attempt_map.remove(key);
    }
    insert_actf_map(story, source_id, &attempt_prefix, attempt_map)?;

    let trajectory_prefix = pointer_join(&attempt_prefix, "trajectory");
    let mut trajectory_value = serde_json::to_value(&attempt.trajectory)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    let trajectory_map = trajectory_value.as_object_mut().ok_or_else(|| {
        crate::InputIssue::invalid("serialized ACTF trajectory must be an object")
    })?;
    for key in ["schema_version", "steps"] {
        trajectory_map.remove(key);
    }
    insert_actf_map(story, source_id, &trajectory_prefix, trajectory_map)?;

    for (step_index, step) in attempt.trajectory.steps.iter().enumerate() {
        let step_prefix = pointer_join(
            &pointer_join(&trajectory_prefix, "steps"),
            &step_index.to_string(),
        );
        let mut step_value = serde_json::to_value(step)
            .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
        let step_map = step_value
            .as_object_mut()
            .ok_or_else(|| crate::InputIssue::invalid("serialized ACTF step must be an object"))?;
        let assistant = step_map.remove("assistant_content");
        for key in [
            "step_id",
            "assistant_content",
            "metric",
            "tools",
            "observation",
            "started_at",
        ] {
            step_map.remove(key);
        }
        insert_actf_map(story, source_id, &step_prefix, step_map)?;
        if let Some(mut assistant) = assistant {
            let assistant = assistant.as_object_mut().ok_or_else(|| {
                crate::InputIssue::invalid("serialized ACTF assistant content must be an object")
            })?;
            for key in ["content", "reasoning_content", "tool_calls"] {
                assistant.remove(key);
            }
            insert_actf_map(
                story,
                source_id,
                &pointer_join(&step_prefix, "assistant_content"),
                assistant,
            )?;
        }
        for (call_index, call) in step.tools.iter().enumerate() {
            capture_actf_tool(
                story,
                source_id,
                &pointer_join(
                    &pointer_join(&step_prefix, "tools"),
                    &call_index.to_string(),
                ),
                call,
            )?;
        }
        for (call_index, call) in step.assistant_content.tool_calls.iter().enumerate() {
            capture_actf_tool(
                story,
                source_id,
                &pointer_join(
                    &pointer_join(
                        &pointer_join(&step_prefix, "assistant_content"),
                        "tool_calls",
                    ),
                    &call_index.to_string(),
                ),
                call,
            )?;
        }
    }
    Ok(())
}

fn capture_actf_tool(
    story: &mut StorylineDocument,
    source_id: &str,
    prefix: &str,
    call: &ActfToolCall,
) -> crate::InputResult<()> {
    story.unknown_fields.insert(
        "actf",
        source_id,
        pointer_join(prefix, "type"),
        Value::String(call.kind.clone()),
    )?;
    let mut unknown = call.extra.clone();
    for key in ["name", "input", "command", "aggregated_output"] {
        unknown.remove(key);
    }
    insert_actf_map(story, source_id, prefix, &unknown)
}

fn insert_actf_map(
    story: &mut StorylineDocument,
    source_id: &str,
    prefix: &str,
    fields: &Map<String, Value>,
) -> crate::InputResult<()> {
    for (key, value) in fields {
        story
            .unknown_fields
            .insert("actf", source_id, pointer_join(prefix, key), value.clone())?;
    }
    Ok(())
}

fn pointer_join(parent: &str, token: &str) -> String {
    format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1"))
}

fn storylines_to_actf_pointer(stories: &[StorylineDocument]) -> Result<ActfDocument> {
    if stories.is_empty() {
        anyhow::bail!("ACTF conversion requires at least one Storyline");
    }
    let task_id = stories[0]
        .run_id
        .clone()
        .unwrap_or_else(|| stories[0].session_id.clone());
    let mut attempts = Map::new();
    let mut carriers = Vec::new();
    for (story_index, story) in stories.iter().enumerate() {
        let attempt_id = story
            .attempt_id
            .as_deref()
            .or_else(|| (stories.len() == 1).then_some("1"))
            .ok_or_else(|| anyhow::anyhow!("ACTF multi-attempt Storyline requires attempt_id"))?;
        let canonical = synthesize_actf(story)?;
        let attempt = serde_json::to_value(&canonical.attempts["1"])?;
        if attempts.insert(attempt_id.into(), attempt).is_some() {
            anyhow::bail!("duplicate ACTF attempt id '{attempt_id}'");
        }
        carriers.push(CarrierBinding {
            story_index,
            pointer: pointer_join("/attempts", attempt_id),
        });
    }
    let task_correct = stories[0]
        .final_metrics
        .as_ref()
        .and_then(|metrics| metrics.get("task_correct"))
        .cloned()
        .unwrap_or(Value::Bool(false));
    let mut value = json!({
        "task_id": task_id,
        "category": "unknown",
        "k": stories.len(),
        "correct": task_correct,
        "attempts_tried": stories.len(),
        "solved_at": Value::Null,
        "attempts": attempts,
    });
    let mut source_id = None::<String>;
    let mut unknown_fields = BTreeMap::<String, Value>::new();
    let actf_sources = stories
        .iter()
        .filter_map(|story| story.unknown_fields.sources.get("actf"))
        .collect::<Vec<_>>();
    if !actf_sources.is_empty() && actf_sources.len() != stories.len() {
        anyhow::bail!("cannot mix ACTF unknown fields and unrelated Storylines");
    }
    for source in actf_sources {
        if source_id
            .as_ref()
            .is_some_and(|id| id != &source.source_document_id)
        {
            anyhow::bail!("one ACTF document cannot merge multiple source documents");
        }
        source_id = Some(source.source_document_id.clone());
        for (pointer, field_value) in &source.fields {
            match unknown_fields.get(pointer) {
                Some(existing) if existing != field_value => {
                    anyhow::bail!("ACTF unknown-field conflict at '{pointer}'")
                }
                Some(_) => {}
                None => {
                    unknown_fields.insert(pointer.clone(), field_value.clone());
                }
            }
        }
    }
    for (pointer, field_value) in unknown_fields {
        let write = if is_actf_source_owned(&pointer) {
            PointerWrite::ReplaceSourceOwned
        } else {
            PointerWrite::InsertOnly
        };
        restore_json_pointer(&mut value, &pointer, field_value, write)
            .with_context(|| format!("restore ACTF unknown field '{pointer}'"))?;
    }
    write_foreign_unknown_fields_envelope(DocumentFormat::Actf, &mut value, stories, &carriers)?;
    let document: ActfDocument = serde_json::from_value(value)?;
    document.validate()?;
    Ok(document)
}

fn is_actf_source_owned(pointer: &str) -> bool {
    let Ok(tokens) = decode_json_pointer(pointer) else {
        return false;
    };
    if tokens.len() == 1 {
        return matches!(
            tokens[0].as_str(),
            "category" | "k" | "attempts_tried" | "solved_at"
        );
    }
    let Some(last) = tokens.last().map(String::as_str) else {
        return false;
    };
    matches!(
        last,
        "final_answer"
            | "ground_truth"
            | "error"
            | "artifacts"
            | "extra"
            | "analysis_result"
            | "meta"
            | "schema_version"
            | "started_at"
            | "finished_at"
            | "system_prompt"
            | "user_content"
            | "type"
    )
}

fn synthesize_actf(story: &StorylineDocument) -> Result<ActfDocument> {
    if story.session_id.is_empty() || story.agent.id.is_empty() {
        anyhow::bail!("invalid Storyline identity for ACTF conversion");
    }
    let epoch = "1970-01-01 00:00:00+00:00".to_string();
    let started_at = story
        .turns
        .first()
        .and_then(|turn| turn.timestamp.as_ref())
        .map(format_actf_timestamp)
        .transpose()?
        .unwrap_or_else(|| epoch.clone());
    let finished_at = story
        .turns
        .last()
        .and_then(|turn| turn.timestamp.as_ref())
        .map(format_actf_timestamp)
        .transpose()?
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
        analysis_result: story
            .final_metrics
            .as_ref()
            .and_then(|metrics| metrics.get("analysis_result"))
            .cloned()
            .unwrap_or_else(|| json!({})),
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
    let step = serde_json::from_value(storyline_step_value(turn)?)?;
    Ok(step)
}

fn storyline_tool_to_actf(call: &StorylineToolCall) -> Value {
    let mut tool = Map::new();
    tool.insert("id".into(), Value::String(call.tool_call_id.clone()));
    if call.function_name == "command_execution" {
        tool.insert("type".into(), Value::String("command_execution".into()));
        if let Some(command) = call.arguments.get("command") {
            tool.insert("command".into(), command.clone());
        }
        if let Some(result) = &call.result {
            tool.insert("aggregated_output".into(), result.clone());
        }
    } else {
        tool.insert("type".into(), Value::String("tool_use".into()));
        tool.insert("name".into(), Value::String(call.function_name.clone()));
        tool.insert("input".into(), call.arguments.clone());
    }
    Value::Object(tool)
}

fn storyline_observation_to_actf(result: &Value) -> Value {
    let mut extra = result.as_object().cloned().unwrap_or_default();
    let source_call_id = extra.remove("source_call_id");
    if extra.get("type").and_then(Value::as_str) == Some("command_execution") {
        if let Some(content) = extra.remove("content") {
            extra.insert("aggregated_output".into(), content);
        }
    }
    extra
        .entry("type")
        .or_insert_with(|| Value::String("tool_result".into()));
    if let Some(source_call_id) = source_call_id {
        if extra.contains_key("tool_use_id") {
            extra.insert("tool_use_id".into(), source_call_id);
        } else if extra.contains_key("id") {
            extra.insert("id".into(), source_call_id);
        } else {
            extra.insert("tool_use_id".into(), source_call_id);
        }
    }
    Value::Object(extra)
}

fn storyline_step_value(turn: &StorylineTurn) -> Result<Value> {
    let tools = turn
        .tool_calls
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(storyline_tool_to_actf)
        .collect::<Vec<_>>();
    let observations = turn
        .observation
        .as_ref()
        .and_then(|observation| observation.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(storyline_observation_to_actf)
        .collect::<Vec<_>>();
    let mut metric = turn
        .metrics
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let prompt_tokens = metric.get("prompt_tokens").cloned().unwrap_or(json!(0));
    let completion_tokens = metric.get("completion_tokens").cloned().unwrap_or(json!(0));
    let llm_infer_ms = metric
        .get("total_latency_ms")
        .cloned()
        .or_else(|| turn.latency_ms.map(|value| json!(value)))
        .unwrap_or(Value::Null);
    let stop_reason = metric.get("finish_reason").cloned().unwrap_or(Value::Null);
    metric.entry("prompt_tokens_len").or_insert(prompt_tokens);
    metric
        .entry("completion_tokens_len")
        .or_insert(completion_tokens);
    metric.entry("llm_infer_ms").or_insert(llm_infer_ms);
    metric.entry("env_action_ms").or_insert(Value::Null);
    metric.entry("stop_reason").or_insert(stop_reason);
    let timestamp = turn
        .timestamp
        .as_ref()
        .map(format_actf_timestamp)
        .transpose()?
        .unwrap_or_else(|| "1970-01-01 00:00:00+00:00".into());
    let mut assistant = Map::new();
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
    step.insert("metric".into(), Value::Object(metric));
    step.insert("tools".into(), Value::Array(tools));
    step.insert("observation".into(), Value::Array(observations));
    step.insert("started_at".into(), Value::String(timestamp.clone()));
    step.insert("system_prompt".into(), Value::String(String::new()));
    step.insert("user_content".into(), Value::String(String::new()));
    step.insert("finished_at".into(), Value::String(timestamp.clone()));
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

fn format_actf_timestamp(value: &StorylineTimestamp) -> Result<String> {
    if let Some(source) = value.source_string() {
        return Ok(source.to_string());
    }
    Ok(value
        .instant()
        .format("%Y-%m-%d %H:%M:%S%.f%:z")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::actf::parse_actf_document;
    #[cfg(feature = "lance-store")]
    use crate::store::StorylineLanceStore;

    fn actf_to_storyline(document: &ActfDocument) -> Result<StorylineDocument> {
        let mut stories = actf_to_storylines(document)?;
        anyhow::ensure!(
            stories.len() == 1,
            "test fixture contains {} ACTF attempts",
            stories.len()
        );
        Ok(stories.remove(0))
    }

    fn storyline_to_actf(story: &StorylineDocument) -> Result<ActfDocument> {
        storylines_to_actf(std::slice::from_ref(story))
    }

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
    fn actf_noncanonical_source_fields_are_unknown_without_source_extra() {
        let document = parse_actf_document(
            r#"{
            "task_id": "task-command",
            "category": "software-engineering",
            "k": 1,
            "correct": false,
            "attempts_tried": 1,
            "solved_at": null,
            "retry_count": 2,
            "retry_counts": {"environment": 2},
            "vendor_root": {"kept": true},
            "attempts": {"1": {
                "correct": false,
                "final_answer": null,
                "ground_truth": "expected",
                "status": "completed",
                "score": null,
                "error": "",
                "artifacts": {},
                "extra": {"harness_metrics": {"passed": 0}},
                "analysis_result": {"quality": 7},
                "meta": {"suite": "fixture"},
                "trajectory": {
                    "schema_version": "ACTF_v1.0",
                    "started_at": "2026-01-01 00:00:00+00:00",
                    "finished_at": "2026-01-01 00:00:01+00:00",
                    "steps": [{
                        "step_id": 1,
                        "assistant_content": {
                            "content": "done",
                            "reasoning_content": "think",
                            "tool_calls": [{
                                "type": "command_execution",
                                "id": "cmd-1",
                                "command": "pwd",
                                "aggregated_output": "/app\n",
                                "exit_code": 0,
                                "status": "completed"
                            }]
                        },
                        "metric": {
                            "prompt_tokens_len": 1,
                            "completion_tokens_len": 2,
                            "llm_infer_ms": 3,
                            "env_action_ms": 4,
                            "stop_reason": "stop"
                        },
                        "system_prompt": "system",
                        "user_content": "task",
                        "tools": [{
                            "type": "command_execution",
                            "id": "cmd-1",
                            "command": "pwd",
                            "aggregated_output": "/app\n",
                            "exit_code": 0,
                            "status": "completed"
                        }],
                        "observation": [{
                            "type": "command_execution",
                            "id": "cmd-1",
                            "command": "pwd",
                            "aggregated_output": "/app\n",
                            "exit_code": 0,
                            "status": "completed"
                        }],
                        "started_at": "2026-01-01 00:00:00+00:00",
                        "finished_at": "2026-01-01 00:00:01+00:00"
                    }]
                }
            }}
        }"#,
        )
        .unwrap();

        let story = actf_to_storyline(&document).unwrap();
        let fields = &story.unknown_fields.sources["actf"].fields;
        assert_eq!(fields["/vendor_root"], json!({"kept": true}));
        for pointer in [
            "/category",
            "/k",
            "/attempts_tried",
            "/solved_at",
            "/retry_count",
            "/retry_counts",
            "/attempts/1/final_answer",
            "/attempts/1/ground_truth",
            "/attempts/1/error",
            "/attempts/1/artifacts",
            "/attempts/1/extra",
            "/attempts/1/meta",
            "/attempts/1/trajectory/started_at",
            "/attempts/1/trajectory/finished_at",
            "/attempts/1/trajectory/steps/0/system_prompt",
            "/attempts/1/trajectory/steps/0/user_content",
            "/attempts/1/trajectory/steps/0/finished_at",
            "/attempts/1/trajectory/steps/0/tools/0/type",
            "/attempts/1/trajectory/steps/0/tools/0/exit_code",
            "/attempts/1/trajectory/steps/0/tools/0/status",
        ] {
            assert!(
                fields.contains_key(pointer),
                "missing unknown pointer {pointer}"
            );
        }
        for pointer in [
            "/task_id",
            "/correct",
            "/attempts/1/correct",
            "/attempts/1/status",
            "/attempts/1/score",
            "/attempts/1/analysis_result",
            "/attempts/1/trajectory/schema_version",
            "/attempts/1/trajectory/steps/0/step_id",
            "/attempts/1/trajectory/steps/0/started_at",
            "/attempts/1/trajectory/steps/0/assistant_content/content",
            "/attempts/1/trajectory/steps/0/assistant_content/reasoning_content",
            "/attempts/1/trajectory/steps/0/tools/0/id",
            "/attempts/1/trajectory/steps/0/tools/0/command",
            "/attempts/1/trajectory/steps/0/tools/0/aggregated_output",
        ] {
            assert!(
                !fields.contains_key(pointer),
                "canonical pointer leaked into unknowns: {pointer}"
            );
        }
        assert!(story.extra.is_none());
        assert!(story.turns[0].extra.is_none());
        assert_eq!(
            story.final_metrics.as_ref().unwrap()["analysis_result"]["quality"],
            7
        );
        let call = &story.turns[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.result.as_ref().unwrap(), "/app\n");
        assert!(call.extra.is_none());
        assert_eq!(
            story.turns[0].observation.as_ref().unwrap()["results"][0]["content"],
            "/app\n"
        );

        assert_eq!(storyline_to_actf(&story).unwrap(), document);
    }

    #[test]
    fn actf_unknown_fields_preserve_values_but_storyline_fields_are_authoritative() {
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
            .contains(&["_pchron", "icle_"].concat()));
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
    fn actf_unknown_fields_use_namespaced_exact_paths() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["root_unknown"] = Value::Null;
        value["attempts"]["1"]["trajectory"]["steps"][0]["step_unknown"] = json!({"x": 1});
        value["attempts"]["1"]["trajectory"]["steps"][0]["0"] = json!("literal");
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let stories = actf_to_storylines(&document).unwrap();
        let source = &stories[0].unknown_fields.sources["actf"];
        assert_eq!(source.fields["/root_unknown"], Value::Null);
        assert_eq!(
            source.fields["/attempts/1/trajectory/steps/0/step_unknown"],
            json!({"x": 1})
        );
        assert_eq!(
            stories[0].unknown_key_counts["actf"]["/attempts/1/trajectory/steps/*/0"],
            1
        );
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

    #[test]
    fn synthesis_completes_partial_metrics_and_normalizes_observations() {
        let mut story = StorylineDocument::new("session", "agent");
        story.turns.push(StorylineTurn {
            id: 1,
            kind: Some("autonomous".into()),
            timestamp: None,
            source: "agent".into(),
            message: json!("done"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: Some(vec![StorylineToolCall {
                tool_call_id: "call-1".into(),
                function_name: "inspect".into(),
                arguments: json!({"path": "/tmp"}),
                result: None,
                duration_ms: None,
                extra: None,
            }]),
            observation: Some(json!({
                "results": [{"source_call_id": "call-1", "content": "ok"}]
            })),
            metrics: Some(json!({"reward": 1.0})),
            model_name: None,
            llm_call_count: Some(1),
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
        });

        let document = storyline_to_actf(&story).unwrap();
        let step = &document.attempts["1"].trajectory.steps[0];
        assert_eq!(step.observation[0].kind, "tool_result");
        assert_eq!(step.observation[0].extra["tool_use_id"], "call-1");
        assert_eq!(step.metric.prompt_tokens_len, 0);
        assert_eq!(step.metric.completion_tokens_len, 0);
        assert_eq!(step.metric.extra["reward"], 1.0);
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

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn fractional_space_timestamp_is_lossless_through_lance() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["started_at"] =
            json!("2026-01-01 00:00:00.123456+00:00");
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        document.validate().unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let store = StorylineLanceStore::open(temporary.path()).await.unwrap();

        store.replace_storyline(&story).await.unwrap();
        let restored = store
            .get_storyline_full(&story.session_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(storyline_to_actf(&restored).unwrap(), document);
    }
}
