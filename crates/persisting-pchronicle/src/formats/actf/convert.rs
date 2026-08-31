//! ACTF ⇄ Storyline conversion.

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde_json::{Map, Value, json};

use super::{
    ACTF_SCHEMA_VERSION, ActfAttempt, ActfDocument, ActfObservation, ActfStep, ActfToolCall,
    ActfTrajectory,
};
use crate::Result;
use crate::format::DocumentFormat;
use crate::formats::storyline::{
    STORYLINE_SCHEMA_VERSION, StorylineAgent, StorylineDocument, StorylineEnv, StorylineOrigin,
    StorylinePrompt, StorylineTask, StorylineTaskLlm, StorylineTaskResult, StorylineToolCall,
    StorylineToolResponse, StorylineTurn,
};
use crate::formats::timestamp::StorylineTimestamp;
use crate::formats::unknown_fields::{
    CarrierBinding, PointerWrite, UnknownFieldLimits, decode_json_pointer, insert_unknown_map,
    normalize_actf_pointer, pointer_join, restore_json_pointer, validate_unknown_fields_with,
    write_foreign_unknown_fields_envelope,
};

const ACTF_EXTRA_TASK_CORRECT: &str = "task_correct";
const ACTF_EXTRA_CATEGORY: &str = "category";
const ACTF_EXTRA_ATTEMPTS_TRIED: &str = "attempts_tried";
const ACTF_EXTRA_SOLVED_AT: &str = "solved_at";
const ACTF_EXTRA_RETRY_COUNT: &str = "retry_count";
const ACTF_EXTRA_RETRY_COUNTS: &str = "retry_counts";

fn actf_tool_to_storyline(
    call: &ActfToolCall,
    duration_ms: Option<i64>,
    step_id: i64,
    call_index: usize,
) -> StorylineToolCall {
    let status = call
        .extra
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.is_empty())
        .map(str::to_string);
    let exit_code = call.extra.get("exit_code").and_then(Value::as_i64);
    let response = (status.is_some() || exit_code.is_some())
        .then_some(StorylineToolResponse { status, exit_code });
    StorylineToolCall {
        tool_call_id: call.effective_id(step_id, call_index),
        function_name: actf_tool_name(call),
        arguments: actf_tool_arguments(call),
        result: call.extra.get("aggregated_output").cloned(),
        duration_ms,
        extra: None,
        kind: (!call.kind.is_empty()).then(|| call.kind.clone()),
        response,
    }
}

fn actf_observation_to_storyline_with_call_id(
    observation: &ActfObservation,
    fallback_call_id: Option<&str>,
) -> Value {
    let mut result =
        serde_json::to_value(observation).unwrap_or_else(|_| Value::Object(Map::new()));
    if let Some(object) = result.as_object_mut() {
        if let Some(source_call_id) = actf_observation_call_id(observation).or(fallback_call_id) {
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

fn actf_observation_fallback_call_id(
    observation: &ActfObservation,
    source_tools: &[ActfToolCall],
    step_id: i64,
    assigned: &mut [bool],
) -> Option<String> {
    if actf_observation_call_id(observation).is_some() {
        return None;
    }
    // Runtime records are step-level metadata rather than tool results. Keep
    // them in the authoritative observation column, but do not manufacture a
    // tool-call association for them.
    let is_tool_observation = observation.kind == "tool_result"
        || observation.extra.get("role").and_then(Value::as_str) == Some("tool")
        || observation
            .extra
            .get("tool_names")
            .and_then(Value::as_array)
            .is_some_and(|names| !names.is_empty());
    if !is_tool_observation {
        return None;
    }
    let names = observation
        .extra
        .get("tool_names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let position = source_tools
        .iter()
        .enumerate()
        .find(|(index, call)| {
            !assigned[*index]
                && (names.is_empty() || names.iter().any(|name| *name == actf_tool_name(call)))
        })
        .map(|(index, _)| index)?;
    assigned[position] = true;
    Some(source_tools[position].effective_id(step_id, position))
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
    if !attempt.trajectory.events.is_empty() && attempt.trajectory.steps.is_empty() {
        return event_log_attempt_to_storyline(document, attempt_id, attempt, multiple_attempts);
    }
    let prompt_pairs: Vec<(String, String)> = attempt
        .trajectory
        .steps
        .iter()
        .map(|step| (step.system_prompt.clone(), step.user_content.clone()))
        .collect();
    let baseline = prompt_pairs
        .iter()
        .find(|(system, user)| !system.is_empty() || !user.is_empty())
        .cloned();
    let document_prompt = baseline
        .as_ref()
        .and_then(|(system, user)| StorylinePrompt::from_pair(system, user));
    let mut turns = Vec::with_capacity(attempt.trajectory.steps.len());
    for (step, pair) in attempt.trajectory.steps.iter().zip(prompt_pairs) {
        let source_tools = step.effective_tools();
        let mut assigned_observation_calls = vec![false; source_tools.len()];
        for observation in &step.observation {
            if let Some(call_id) = actf_observation_call_id(observation)
                && let Some(position) = source_tools.iter().enumerate().find_map(|(index, call)| {
                    (call.effective_id(step.step_id, index) == call_id).then_some(index)
                })
            {
                assigned_observation_calls[position] = true;
            }
        }
        let tool_calls = (!source_tools.is_empty())
            .then(|| {
                source_tools
                    .iter()
                    .enumerate()
                    .map(|(call_index, call)| {
                        Ok(actf_tool_to_storyline(
                            call,
                            if source_tools.len() == 1 {
                                step.metric.env_action_ms.as_f64().map(|value| value as i64)
                            } else {
                                None
                            },
                            step.step_id,
                            call_index,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;
        let observation = (!step.observation.is_empty()).then(|| {
            let results = step
                .observation
                .iter()
                .map(|observation| {
                    let fallback_call_id = actf_observation_fallback_call_id(
                        observation,
                        source_tools,
                        step.step_id,
                        &mut assigned_observation_calls,
                    );
                    actf_observation_to_storyline_with_call_id(
                        observation,
                        fallback_call_id.as_deref(),
                    )
                })
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
            env: None,
            prompt: actf_turn_prompt(pair, baseline.as_ref()),
            finished_at: Some(StorylineTimestamp::from_rfc3339(&step.finished_at)?),
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
        task: actf_task(document, attempt),
        prompt: document_prompt,
        started_at: Some(StorylineTimestamp::from_rfc3339(
            &attempt.trajectory.started_at,
        )?),
        finished_at: Some(StorylineTimestamp::from_rfc3339(
            &attempt.trajectory.finished_at,
        )?),
        final_metrics: Some(actf_final_metrics(document, attempt)),
        continued_trajectory_ref: None,
        extra: omit_empty_value(&attempt.extra),
        meta: omit_empty_value(&attempt.meta),
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns,
    })
}

fn event_log_attempt_to_storyline(
    document: &ActfDocument,
    attempt_id: &str,
    attempt: &ActfAttempt,
    multiple_attempts: bool,
) -> Result<StorylineDocument> {
    let (turns, session_env, model_name) = openclaw_events_to_turns(&attempt.trajectory.events)?;
    let session_id = if multiple_attempts {
        format!("{}#attempt-{attempt_id}", document.task_id)
    } else {
        document.task_id.clone()
    };
    let mut task = actf_task(document, attempt).unwrap_or_default();
    if let Some(env) = session_env {
        task.env = Some(env);
    }
    event_log_storyline(
        document,
        attempt,
        attempt_id,
        session_id,
        model_name,
        (!task.is_empty()).then_some(task),
        turns,
    )
}

fn event_log_storyline(
    document: &ActfDocument,
    attempt: &ActfAttempt,
    attempt_id: &str,
    session_id: String,
    model_name: Option<String>,
    task: Option<StorylineTask>,
    turns: Vec<StorylineTurn>,
) -> Result<StorylineDocument> {
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
            model_name,
            tool_definitions: None,
            extra: None,
        },
        parent: None,
        child_session_ids: None,
        notes: None,
        task,
        prompt: None,
        started_at: Some(StorylineTimestamp::from_rfc3339(
            &attempt.trajectory.started_at,
        )?),
        finished_at: Some(StorylineTimestamp::from_rfc3339(
            &attempt.trajectory.finished_at,
        )?),
        final_metrics: Some(actf_final_metrics(document, attempt)),
        continued_trajectory_ref: None,
        extra: omit_empty_value(&attempt.extra),
        meta: omit_empty_value(&attempt.meta),
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns,
    })
}

fn openclaw_events_to_turns(
    events: &[Value],
) -> Result<(Vec<StorylineTurn>, Option<StorylineEnv>, Option<String>)> {
    let mut turns = Vec::new();
    let mut session_env = None;
    let mut model_name = None;
    let mut next_id = 1i64;
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some("session") => {
                let mut env = StorylineEnv {
                    id: event
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string),
                    ..StorylineEnv::default()
                };
                if let Some(cwd) = event.get("cwd").and_then(Value::as_str) {
                    let mut state = serde_json::Map::new();
                    state.insert("cwd".into(), Value::String(cwd.to_string()));
                    env.state = Some(state);
                }
                if !env.is_empty() {
                    session_env = Some(env);
                }
            }
            Some("model_change") => {
                model_name = event
                    .get("modelId")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string);
            }
            Some("message") => {
                if let Some(turn) = openclaw_message_to_turn(event, next_id)? {
                    next_id = next_id
                        .checked_add(1)
                        .context("ACTF event-log turn id overflow")?;
                    turns.push(turn);
                }
            }
            _ => {}
        }
    }
    attach_openclaw_tool_results(&mut turns);
    Ok((turns, session_env, model_name))
}

fn openclaw_message_to_turn(event: &Value, id: i64) -> Result<Option<StorylineTurn>> {
    let message = event.get("message").and_then(Value::as_object);
    let Some(message) = message else {
        return Ok(None);
    };
    let role = message.get("role").and_then(Value::as_str).unwrap_or("");
    let timestamp = event
        .get("timestamp")
        .and_then(Value::as_str)
        .map(StorylineTimestamp::from_rfc3339)
        .transpose()?;
    let content = message.get("content").cloned().unwrap_or(Value::Null);
    match role {
        "user" => Ok(Some(StorylineTurn {
            id,
            kind: None,
            timestamp,
            source: "user".into(),
            message: Value::String(openclaw_text_parts(&content, "text")),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: message
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        })),
        "assistant" => {
            let tool_calls = openclaw_tool_calls(&content);
            Ok(Some(StorylineTurn {
                id,
                kind: (!tool_calls.is_empty()).then(|| "autonomous".into()),
                timestamp,
                source: "agent".into(),
                message: Value::String(openclaw_text_parts(&content, "text")),
                reasoning_content: omit_empty_string(&openclaw_text_parts(&content, "thinking")),
                reasoning_effort: None,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                observation: None,
                metrics: message.get("usage").cloned(),
                model_name: message
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                llm_call_count: Some(1),
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            }))
        }
        "toolResult" => Ok(Some(StorylineTurn {
            id,
            kind: None,
            timestamp,
            source: "agent".into(),
            message: Value::String(openclaw_text_parts(&content, "text")),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: Some(json!({
                "results": [openclaw_tool_result(message, &content)]
            })),
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: Some(json!({
                "openclaw_role": "toolResult",
                "toolCallId": message.get("toolCallId"),
                "toolName": message.get("toolName"),
            })),
            env: None,
            prompt: None,
            finished_at: None,
        })),
        _ => Ok(None),
    }
}

fn openclaw_text_parts(content: &Value, part_type: &str) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some(part_type))
            .filter_map(|part| {
                part.get(part_type)
                    .or_else(|| part.get("text"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn openclaw_tool_calls(content: &Value) -> Vec<StorylineToolCall> {
    let Some(parts) = content.as_array() else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("toolCall"))
        .map(|part| StorylineToolCall {
            tool_call_id: part
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            function_name: part
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            arguments: part.get("arguments").cloned().unwrap_or(json!({})),
            result: None,
            duration_ms: None,
            extra: None,
            kind: Some("function".into()),
            response: None,
        })
        .filter(|call| !call.tool_call_id.is_empty() && !call.function_name.is_empty())
        .collect()
}

fn openclaw_tool_result(message: &Map<String, Value>, content: &Value) -> Value {
    let mut result = Map::new();
    result.insert(
        "content".into(),
        Value::String(openclaw_text_parts(content, "text")),
    );
    if let Some(id) = message.get("toolCallId").cloned() {
        result.insert("tool_use_id".into(), id.clone());
        result.insert("source_call_id".into(), id);
    }
    if let Some(name) = message.get("toolName").cloned() {
        result.insert("name".into(), name);
    }
    if let Some(details) = message.get("details").and_then(Value::as_object) {
        if let Some(status) = details.get("status").cloned() {
            result.insert("status".into(), status);
        }
        if let Some(exit_code) = details.get("exitCode").cloned() {
            result.insert("exit_code".into(), exit_code);
        }
        if let Some(duration_ms) = details.get("durationMs").cloned() {
            result.insert("duration_ms".into(), duration_ms);
        }
        if let Some(aggregated) = details.get("aggregated").cloned() {
            result.insert("aggregated_output".into(), aggregated);
        }
    }
    Value::Object(result)
}

struct OpenclawToolResult {
    id: String,
    text: String,
    duration_ms: Option<i64>,
    status: Option<String>,
    exit_code: Option<i64>,
    result: Value,
}

fn openclaw_pending_result(turn: &StorylineTurn) -> Option<OpenclawToolResult> {
    let extra = turn.extra.as_ref()?;
    if extra.get("openclaw_role").and_then(Value::as_str) != Some("toolResult") {
        return None;
    }
    let result = turn
        .observation
        .as_ref()?
        .get("results")?
        .as_array()?
        .first()?
        .clone();
    Some(OpenclawToolResult {
        id: extra.get("toolCallId").and_then(Value::as_str)?.to_string(),
        text: turn.message.as_str().unwrap_or("").to_string(),
        duration_ms: result.get("duration_ms").and_then(Value::as_i64),
        status: result
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        exit_code: result.get("exit_code").and_then(Value::as_i64),
        result,
    })
}

fn attach_openclaw_tool_results(turns: &mut Vec<StorylineTurn>) {
    let pending = turns
        .iter()
        .filter_map(openclaw_pending_result)
        .collect::<Vec<_>>();
    for item in pending {
        let Some(turn) = turns.iter_mut().rev().find(|turn| {
            turn.tool_calls
                .as_ref()
                .is_some_and(|calls| calls.iter().any(|call| call.tool_call_id == item.id))
        }) else {
            continue;
        };
        if let Some(call) = turn
            .tool_calls
            .as_mut()
            .and_then(|calls| calls.iter_mut().find(|call| call.tool_call_id == item.id))
        {
            call.result = Some(Value::String(item.text));
            call.duration_ms = item.duration_ms;
            if item.status.is_some() || item.exit_code.is_some() {
                call.response = Some(StorylineToolResponse {
                    status: item.status,
                    exit_code: item.exit_code,
                });
            }
        }
        let results = turn
            .observation
            .get_or_insert_with(|| json!({"results": []}));
        if let Some(results) = results.get_mut("results").and_then(Value::as_array_mut) {
            results.push(item.result);
        }
    }
    turns.retain(|turn| openclaw_pending_result(turn).is_none());
}

fn actf_turn_prompt(
    pair: (String, String),
    baseline: Option<&(String, String)>,
) -> Option<StorylinePrompt> {
    match baseline {
        Some(baseline) if pair == *baseline => None,
        Some(_) if pair.0.is_empty() && pair.1.is_empty() => {
            Some(StorylinePrompt::explicit_clear())
        }
        Some(_) | None => StorylinePrompt::from_pair(&pair.0, &pair.1),
    }
}

fn omit_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

fn omit_empty_value(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::String(text) if text.is_empty() => None,
        Value::Object(object) if object.is_empty() => None,
        Value::Array(items) if items.is_empty() => None,
        other => Some(other.clone()),
    }
}

fn actf_task(document: &ActfDocument, attempt: &ActfAttempt) -> Option<StorylineTask> {
    let mut extra = Map::new();
    extra.insert(ACTF_EXTRA_TASK_CORRECT.into(), json!(document.correct));
    if let Some(category) = omit_empty_string(&document.category) {
        extra.insert(ACTF_EXTRA_CATEGORY.into(), json!(category));
    }
    if let Ok(attempts_tried) = i64::try_from(document.attempts_tried) {
        extra.insert(ACTF_EXTRA_ATTEMPTS_TRIED.into(), json!(attempts_tried));
    }
    if let Some(solved_at) = attempt_solved_at(&document.solved_at) {
        extra.insert(ACTF_EXTRA_SOLVED_AT.into(), json!(solved_at));
    }
    if let Some(retry_count) = document.extra.get(ACTF_EXTRA_RETRY_COUNT).cloned() {
        extra.insert(ACTF_EXTRA_RETRY_COUNT.into(), retry_count);
    }
    if let Some(retry_counts) = document.extra.get(ACTF_EXTRA_RETRY_COUNTS).cloned() {
        extra.insert(ACTF_EXTRA_RETRY_COUNTS.into(), retry_counts);
    }
    let result = StorylineTaskResult {
        correct: Some(attempt.correct),
        final_answer: omit_empty_value(&attempt.final_answer),
        ground_truth: omit_empty_value(&attempt.ground_truth),
        status: omit_empty_string(&attempt.status),
        score: omit_empty_value(&attempt.score),
        error: omit_empty_string(&attempt.error),
        artifacts: omit_empty_value(&attempt.artifacts),
        max_score: omit_empty_value(&attempt.max_score),
        extra,
    };
    let llm = StorylineTaskLlm {
        k: i64::try_from(document.k).ok().filter(|k| *k > 0),
    };
    let task = StorylineTask {
        env: None,
        llm: (!llm.is_empty()).then_some(llm),
        result: (!result.is_empty()).then_some(result),
    };
    (!task.is_empty()).then_some(task)
}

fn attempt_solved_at(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn actf_final_metrics(document: &ActfDocument, attempt: &ActfAttempt) -> Value {
    let mut metrics = Map::from_iter([
        ("correct".into(), json!(attempt.correct)),
        ("score".into(), attempt.score.clone()),
        ("status".into(), json!(attempt.status)),
        ("task_correct".into(), json!(document.correct)),
        ("analysis_result".into(), attempt.analysis_result.clone()),
    ]);
    if let Some(max_score) = omit_empty_value(&attempt.max_score) {
        metrics.insert("max_score".into(), max_score);
    }
    Value::Object(metrics)
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
    for key in [
        "category",
        "k",
        "attempts_tried",
        "solved_at",
        "retry_count",
        "retry_counts",
    ] {
        root.remove(key);
    }
    insert_unknown_map(story, "actf", source_id, "", root)?;

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
        "final_answer",
        "ground_truth",
        "error",
        "artifacts",
        "extra",
        "meta",
        "max_score",
    ] {
        attempt_map.remove(key);
    }
    insert_unknown_map(story, "actf", source_id, &attempt_prefix, attempt_map)?;

    let trajectory_prefix = pointer_join(&attempt_prefix, "trajectory");
    let mut trajectory_value = serde_json::to_value(&attempt.trajectory)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    let trajectory_map = trajectory_value.as_object_mut().ok_or_else(|| {
        crate::InputIssue::invalid("serialized ACTF trajectory must be an object")
    })?;
    for key in [
        "schema_version",
        "steps",
        "started_at",
        "finished_at",
        "events",
    ] {
        trajectory_map.remove(key);
    }
    insert_unknown_map(story, "actf", source_id, &trajectory_prefix, trajectory_map)?;

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
            "finished_at",
            "system_prompt",
            "user_content",
        ] {
            step_map.remove(key);
        }
        insert_unknown_map(story, "actf", source_id, &step_prefix, step_map)?;
        if let Some(mut assistant) = assistant {
            let assistant = assistant.as_object_mut().ok_or_else(|| {
                crate::InputIssue::invalid("serialized ACTF assistant content must be an object")
            })?;
            for key in ["content", "reasoning_content", "tool_calls"] {
                assistant.remove(key);
            }
            insert_unknown_map(
                story,
                "actf",
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
    let mut unknown = call.extra.clone();
    for key in [
        "name",
        "input",
        "arguments",
        "command",
        "aggregated_output",
        "status",
        "exit_code",
        "function",
    ] {
        unknown.remove(key);
    }
    insert_unknown_map(story, "actf", source_id, prefix, &unknown)
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
    let task_result = stories[0]
        .task
        .as_ref()
        .and_then(|task| task.result.as_ref());
    let task_correct = task_result
        .and_then(|result| result.extra_bool(ACTF_EXTRA_TASK_CORRECT))
        .map(Value::Bool)
        .or_else(|| {
            stories[0]
                .final_metrics
                .as_ref()
                .and_then(|metrics| metrics.get(ACTF_EXTRA_TASK_CORRECT))
                .cloned()
        })
        .unwrap_or(Value::Bool(false));
    let category = task_result
        .and_then(|result| result.extra_str(ACTF_EXTRA_CATEGORY))
        .unwrap_or("unknown")
        .to_string();
    let k = stories[0]
        .task
        .as_ref()
        .and_then(|task| task.llm.as_ref())
        .and_then(|llm| llm.k)
        .filter(|k| *k > 0)
        .unwrap_or(stories.len() as i64);
    let attempts_tried = task_result
        .and_then(|result| result.extra_i64(ACTF_EXTRA_ATTEMPTS_TRIED))
        .unwrap_or(stories.len() as i64);
    let solved_at = task_result
        .and_then(|result| result.extra_str(ACTF_EXTRA_SOLVED_AT))
        .map(|text| Value::String(text.to_string()))
        .unwrap_or(Value::Null);
    let mut root = Map::from_iter([
        ("task_id".into(), Value::String(task_id)),
        ("category".into(), Value::String(category)),
        ("k".into(), Value::Number(k.into())),
        ("correct".into(), task_correct),
        (
            "attempts_tried".into(),
            Value::Number(attempts_tried.into()),
        ),
        ("solved_at".into(), solved_at),
        ("attempts".into(), Value::Object(attempts)),
    ]);
    if let Some(retry_count) =
        task_result.and_then(|result| result.extra_value(ACTF_EXTRA_RETRY_COUNT).cloned())
    {
        root.insert(ACTF_EXTRA_RETRY_COUNT.into(), retry_count);
    }
    if let Some(retry_counts) =
        task_result.and_then(|result| result.extra_value(ACTF_EXTRA_RETRY_COUNTS).cloned())
    {
        root.insert(ACTF_EXTRA_RETRY_COUNTS.into(), retry_counts);
    }
    let mut value = Value::Object(root);
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
            | "max_score"
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
        .started_at
        .as_ref()
        .map(format_actf_timestamp)
        .transpose()?
        .or_else(|| {
            story
                .turns
                .first()
                .and_then(|turn| turn.timestamp.as_ref())
                .map(format_actf_timestamp)
                .transpose()
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| epoch.clone());
    let finished_at = story
        .finished_at
        .as_ref()
        .map(format_actf_timestamp)
        .transpose()?
        .or_else(|| {
            story
                .turns
                .last()
                .and_then(|turn| turn.finished_at.as_ref().or(turn.timestamp.as_ref()))
                .map(format_actf_timestamp)
                .transpose()
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| started_at.clone());
    let steps = story
        .turns
        .iter()
        .map(|turn| synthesize_step(story, turn))
        .collect::<Result<Vec<_>>>()?;
    let result = story.task.as_ref().and_then(|task| task.result.as_ref());
    let correct = result
        .and_then(|result| result.correct)
        .or_else(|| {
            story
                .final_metrics
                .as_ref()
                .and_then(|metrics| metrics.get("correct"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let score = result
        .and_then(|result| result.score.clone())
        .or_else(|| {
            story
                .final_metrics
                .as_ref()
                .and_then(|metrics| metrics.get("score"))
                .cloned()
        })
        .unwrap_or(Value::Null);
    let status = result
        .and_then(|result| result.status.clone())
        .or_else(|| {
            story
                .final_metrics
                .as_ref()
                .and_then(|metrics| metrics.get("status"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "completed".into());
    let attempt = ActfAttempt {
        correct,
        final_answer: result
            .and_then(|result| result.final_answer.clone())
            .unwrap_or(Value::Null),
        ground_truth: result
            .and_then(|result| result.ground_truth.clone())
            .unwrap_or_else(|| Value::String(String::new())),
        trajectory: ActfTrajectory {
            schema_version: ACTF_SCHEMA_VERSION.into(),
            steps,
            started_at,
            finished_at: finished_at.clone(),
            events: Vec::new(),
            extra: Map::new(),
        },
        status,
        score,
        error: result
            .and_then(|result| result.error.clone())
            .unwrap_or_default(),
        artifacts: result
            .and_then(|result| result.artifacts.clone())
            .unwrap_or_else(|| json!({})),
        extra: story.extra.clone().unwrap_or_else(|| json!({})),
        analysis_result: story
            .final_metrics
            .as_ref()
            .and_then(|metrics| metrics.get("analysis_result"))
            .cloned()
            .unwrap_or_else(|| json!({})),
        meta: story.meta.clone().unwrap_or_else(|| json!({})),
        max_score: result
            .and_then(|result| result.max_score.clone())
            .unwrap_or(Value::Null),
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

fn synthesize_step(story: &StorylineDocument, turn: &StorylineTurn) -> Result<ActfStep> {
    let step = serde_json::from_value(storyline_step_value(story, turn)?)?;
    Ok(step)
}

fn storyline_tool_to_actf(call: &StorylineToolCall) -> Value {
    let mut tool = Map::new();
    tool.insert("id".into(), Value::String(call.tool_call_id.clone()));
    let kind = call.kind.clone().unwrap_or_else(|| {
        if call.function_name == "command_execution" {
            "command_execution".into()
        } else {
            "tool_use".into()
        }
    });
    tool.insert("type".into(), Value::String(kind.clone()));
    if kind == "command_execution" {
        if let Some(command) = call.arguments.get("command") {
            tool.insert("command".into(), command.clone());
        }
        if let Some(result) = &call.result {
            tool.insert("aggregated_output".into(), result.clone());
        }
    } else {
        tool.insert("name".into(), Value::String(call.function_name.clone()));
        tool.insert("input".into(), call.arguments.clone());
    }
    if let Some(response) = &call.response {
        if let Some(status) = &response.status {
            tool.insert("status".into(), Value::String(status.clone()));
        }
        if let Some(exit_code) = response.exit_code {
            tool.insert("exit_code".into(), json!(exit_code));
        }
    }
    Value::Object(tool)
}

fn storyline_observation_to_actf(result: &Value) -> Value {
    let mut extra = result.as_object().cloned().unwrap_or_default();
    let source_call_id = extra.remove("source_call_id");
    if extra.get("type").and_then(Value::as_str) == Some("command_execution")
        && let Some(content) = extra.remove("content")
    {
        extra.insert("aggregated_output".into(), content);
    }
    if let Some(source_call_id) = source_call_id {
        if extra.contains_key("tool_use_id") {
            extra.insert("tool_use_id".into(), source_call_id);
        } else if extra.contains_key("id") {
            extra.insert("id".into(), source_call_id);
        } else {
            extra.insert("tool_use_id".into(), source_call_id);
        }
    }
    if extra.get("type").is_none()
        && (extra.contains_key("tool_use_id") || extra.contains_key("id"))
    {
        extra.insert("type".into(), Value::String("tool_result".into()));
    }
    Value::Object(extra)
}

fn storyline_step_value(story: &StorylineDocument, turn: &StorylineTurn) -> Result<Value> {
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
    let finished_at = turn
        .finished_at
        .as_ref()
        .map(format_actf_timestamp)
        .transpose()?
        .unwrap_or_else(|| timestamp.clone());
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
    step.insert("started_at".into(), Value::String(timestamp));
    let (system_prompt, user_content) = story
        .effective_prompt(turn)
        .map(StorylinePrompt::pair)
        .unwrap_or_default();
    step.insert("system_prompt".into(), Value::String(system_prompt));
    step.insert("user_content".into(), Value::String(user_content));
    step.insert("finished_at".into(), Value::String(finished_at));
    Ok(Value::Object(step))
}

fn actf_tool_name(call: &ActfToolCall) -> String {
    call.extra
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            call.extra
                .get("function")
                .and_then(Value::as_object)
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or(&call.kind)
        .to_string()
}

fn actf_tool_arguments(call: &ActfToolCall) -> Value {
    if let Some(input) = call.extra.get("input") {
        return input.clone();
    }
    if let Some(arguments) = call.extra.get("arguments") {
        return arguments.clone();
    }
    if let Some(command) = call.extra.get("command") {
        return json!({ "command": command });
    }
    if let Some(arguments) = call
        .extra
        .get("function")
        .and_then(Value::as_object)
        .and_then(|function| function.get("arguments"))
    {
        return arguments.clone();
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

    #[cfg(feature = "proptest")]
    mod proptests {
        use proptest::prelude::*;

        use super::*;

        fn token_strategy() -> impl Strategy<Value = String> {
            proptest::string::string_regex("[a-zA-Z0-9._-]{1,24}").unwrap()
        }

        proptest! {
            #[test]
            fn actf_tool_conversion_uses_explicit_or_derived_ids(
                explicit_id in proptest::string::string_regex("[a-zA-Z0-9._-]{0,24}").unwrap(),
                step_id in any::<i64>(),
                call_index in 0usize..32,
                duration_ms in prop::option::of(any::<i64>()),
            ) {
                let call = ActfToolCall {
                    kind: "tool_use".into(),
                    id: explicit_id.clone(),
                    extra: Map::new(),
                };
                let converted = actf_tool_to_storyline(&call, duration_ms, step_id, call_index);
                let expected = if explicit_id.trim().is_empty() {
                    format!("step-{step_id}-tool-{call_index}")
                } else {
                    explicit_id
                };
                prop_assert_eq!(converted.tool_call_id, expected);
                prop_assert_eq!(converted.duration_ms, duration_ms);
            }

            #[test]
            fn observation_conversion_prefers_tool_use_id_over_legacy_id(
                tool_use_id in prop::option::of(token_strategy()),
                legacy_id in prop::option::of(token_strategy()),
            ) {
                let mut extra = Map::new();
                if let Some(value) = &tool_use_id {
                    extra.insert("tool_use_id".into(), Value::String(value.clone()));
                }
                if let Some(value) = &legacy_id {
                    extra.insert("id".into(), Value::String(value.clone()));
                }
                let observation = ActfObservation { kind: "tool_result".into(), extra };
                let converted = actf_observation_to_storyline_with_call_id(&observation, None);
                let expected = tool_use_id.as_deref().or(legacy_id.as_deref());
                prop_assert_eq!(converted.get("source_call_id").and_then(Value::as_str), expected);
            }

            #[test]
            fn observation_conversion_prefers_aggregated_output_over_content(
                aggregated in prop::option::of(token_strategy()),
                content in prop::option::of(token_strategy()),
            ) {
                let mut extra = Map::new();
                if let Some(value) = &aggregated {
                    extra.insert("aggregated_output".into(), Value::String(value.clone()));
                }
                if let Some(value) = &content {
                    extra.insert("content".into(), Value::String(value.clone()));
                }
                let observation = ActfObservation { kind: "tool_result".into(), extra };
                let converted = actf_observation_to_storyline_with_call_id(&observation, None);
                let expected = aggregated.as_deref().or(content.as_deref());
                prop_assert_eq!(converted.get("content").and_then(Value::as_str), expected);
            }
        }
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
    fn actf_event_log_trajectory_maps_openclaw_messages() {
        let document = parse_actf_document(
            r#"{
              "task_id":"gravitational-wave-detection","category":"astronomy","k":1,
              "correct":false,"attempts_tried":1,"solved_at":null,
              "attempts":{"1":{
                "correct":false,"status":"run_error",
                "error":"RunError: timeout",
                "trajectory":[
                  {"type":"session","id":"sess-1","timestamp":"2026-06-17T07:26:27.170Z","cwd":"/root"},
                  {"type":"model_change","timestamp":"2026-06-17T07:26:27.225Z","provider":"vllm","modelId":"qwen"},
                  {"type":"message","timestamp":"2026-06-17T07:26:28Z",
                   "message":{"role":"user","content":[{"type":"text","text":"detect waves"}]}},
                  {"type":"message","timestamp":"2026-06-17T07:26:29Z",
                   "message":{"role":"assistant","model":"qwen","content":[
                     {"type":"thinking","thinking":"plan"},
                     {"type":"text","text":"listing"},
                     {"type":"toolCall","id":"c1","name":"exec","arguments":{"command":"ls"}}
                   ]}},
                  {"type":"message","timestamp":"2026-06-17T07:26:30Z",
                   "message":{"role":"toolResult","toolCallId":"c1","toolName":"exec",
                    "content":[{"type":"text","text":"ok"}],
                    "details":{"status":"completed","exitCode":0,"durationMs":12}}}
                ]
              }}
            }"#,
        )
        .unwrap();
        let story = actf_to_storyline(&document).unwrap();
        assert_eq!(story.session_id, "gravitational-wave-detection");
        assert_eq!(story.agent.model_name.as_deref(), Some("qwen"));
        assert_eq!(
            story
                .task
                .as_ref()
                .unwrap()
                .env
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("sess-1")
        );
        assert_eq!(story.turns.len(), 2);
        assert_eq!(story.turns[0].source, "user");
        assert_eq!(story.turns[0].message, Value::String("detect waves".into()));
        assert_eq!(story.turns[1].source, "agent");
        assert_eq!(story.turns[1].reasoning_content.as_deref(), Some("plan"));
        let call = &story.turns[1].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.tool_call_id, "c1");
        assert_eq!(call.function_name, "exec");
        assert_eq!(call.result.as_ref().unwrap(), "ok");
        assert_eq!(call.duration_ms, Some(12));
        assert_eq!(call.response.as_ref().unwrap().exit_code, Some(0));

        let exported = serde_json::to_value(storyline_to_actf(&story).unwrap()).unwrap();
        assert!(
            exported["attempts"]["1"]["trajectory"].is_object(),
            "OpenClaw event-log import is a lossy ACTF entry; export is canonical object"
        );
        assert!(!exported["attempts"]["1"]["trajectory"].is_array());
    }

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
                "max_score": 10,
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
        assert_eq!(story.extra, Some(json!({"harness_metrics": {"passed": 0}})));
        assert_eq!(story.meta, Some(json!({"suite": "fixture"})));
        assert_eq!(
            story
                .task
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .max_score,
            Some(json!(10))
        );
        assert_eq!(
            story.prompt.as_ref().map(StorylinePrompt::pair),
            Some(("system".into(), "task".into()))
        );
        assert_eq!(story.turns[0].prompt, None);
        assert_eq!(story.turns[0].message, json!("done"));
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
            "/attempts/1/trajectory/started_at",
            "/attempts/1/trajectory/finished_at",
            "/attempts/1/trajectory/steps/0/finished_at",
            "/attempts/1/trajectory/steps/0/tools/0/type",
            "/attempts/1/trajectory/steps/0/tools/0/exit_code",
            "/attempts/1/trajectory/steps/0/tools/0/status",
            "/task_id",
            "/correct",
            "/attempts/1/correct",
            "/attempts/1/status",
            "/attempts/1/score",
            "/attempts/1/extra",
            "/attempts/1/meta",
            "/attempts/1/max_score",
            "/attempts/1/analysis_result",
            "/attempts/1/trajectory/schema_version",
            "/attempts/1/trajectory/steps/0/step_id",
            "/attempts/1/trajectory/steps/0/started_at",
            "/attempts/1/trajectory/steps/0/system_prompt",
            "/attempts/1/trajectory/steps/0/user_content",
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
        assert!(story.turns[0].extra.is_none());
        assert_eq!(
            story.final_metrics.as_ref().unwrap()["analysis_result"]["quality"],
            7
        );
        let call = &story.turns[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.result.as_ref().unwrap(), "/app\n");
        assert_eq!(call.kind.as_deref(), Some("command_execution"));
        assert_eq!(call.response.as_ref().unwrap().exit_code, Some(0));
        assert_eq!(
            call.response.as_ref().unwrap().status.as_deref(),
            Some("completed")
        );
        assert!(call.extra.is_none());
        let task = story.task.as_ref().unwrap();
        assert_eq!(task.llm.as_ref().unwrap().k, Some(1));
        assert_eq!(
            task.result.as_ref().unwrap().extra_str("category"),
            Some("software-engineering")
        );
        assert_eq!(
            task.result.as_ref().unwrap().extra_value("retry_count"),
            Some(&json!(2))
        );
        assert_eq!(
            story.turns[0].observation.as_ref().unwrap()["results"][0]["content"],
            "/app\n"
        );

        assert_eq!(storyline_to_actf(&story).unwrap(), document);
    }

    #[test]
    fn actf_name_arguments_tool_maps_without_type_or_id() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        let tool = json!({"name": "Glob", "arguments": {"path": "/tmp", "pattern": "**/*"}});
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"] = json!([tool]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"] =
            json!([tool]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["observation"] =
            json!([{"role": "tool", "text": "listed"}]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let call = &story.turns[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.function_name, "Glob");
        assert_eq!(call.arguments["pattern"], "**/*");
        assert_eq!(call.tool_call_id, "step-1-tool-0");
        assert!(call.kind.is_none());
    }

    #[test]
    fn actf_object_ground_truth_roundtrips() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["ground_truth"] = json!({"checklist_path": "/tmp/check.json"});
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        assert_eq!(
            story
                .task
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap()
                .ground_truth,
            Some(json!({"checklist_path": "/tmp/check.json"}))
        );
        assert_eq!(
            storyline_to_actf(&story).unwrap().attempts["1"].ground_truth,
            json!({"checklist_path": "/tmp/check.json"})
        );
    }

    #[test]
    fn actf_empty_tools_falls_back_to_assistant_function_calls() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"] = json!([]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"] = json!([{
            "id": "c1",
            "type": "function",
            "function": {
                "name": "bash_command",
                "arguments": {"keystrokes": "pwd\n", "duration": 0.1}
            }
        }]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let call = &story.turns[0].tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.tool_call_id, "c1");
        assert_eq!(call.function_name, "bash_command");
        assert_eq!(call.kind.as_deref(), Some("function"));
        assert_eq!(call.arguments["keystrokes"], "pwd\n");
        assert_eq!(call.arguments["duration"], 0.1);
        assert!(
            !story
                .unknown_fields
                .sources
                .get("actf")
                .map(|source| source
                    .fields
                    .keys()
                    .any(|pointer| pointer.contains("/function")))
                .unwrap_or(false),
            "OpenAI function wrapper should be consumed"
        );
    }

    #[test]
    fn actf_content_only_observation_keeps_missing_type() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["observation"] =
            json!([{"content": "env output"}]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let result = &story.turns[0].observation.as_ref().unwrap()["results"][0];
        assert_eq!(result["content"], "env output");
        assert!(result.get("type").is_none());
        let recovered = storyline_to_actf(&story).unwrap();
        assert_eq!(
            recovered.attempts["1"].trajectory.steps[0].observation[0].kind,
            ""
        );
        assert_eq!(
            recovered.attempts["1"].trajectory.steps[0].observation[0].extra["content"],
            "env output"
        );
    }

    #[test]
    fn actf_null_reasoning_content_omits_reason() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["reasoning_content"] =
            Value::Null;
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        assert!(story.turns[0].reasoning_content.is_none());
        assert_eq!(
            storyline_to_actf(&story).unwrap().attempts["1"]
                .trajectory
                .steps[0]
                .assistant_content
                .reasoning_content,
            ""
        );
    }

    #[test]
    fn actf_unknown_fields_preserve_values_but_storyline_fields_are_authoritative() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["root_unknown"] = Value::Null;
        value["attempts"]["1"]["attempt_unknown"] = json!([3, 2, 1]);
        value["attempts"]["1"]["trajectory"]["trajectory_unknown"] = json!({"x": 1});
        value["attempts"]["1"]["trajectory"]["steps"][0]["step_unknown"] = Value::Null;
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["assistant_unknown"] =
            json!("kept");
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"][0]["tool_unknown"] = Value::Null;
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"][0]["tool_unknown"] =
            Value::Null;
        let document: ActfDocument = serde_json::from_value(value).unwrap();

        let mut story = actf_to_storyline(&document).unwrap();
        assert!(
            !serde_json::to_string(&story)
                .unwrap()
                .contains(&["_pchron", "icle_"].concat())
        );
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
    fn actf_prompt_uses_document_baseline_and_turn_overlay() {
        let mut document = parse_actf_document(FIXTURE).unwrap();
        let steps = &mut document.attempts.get_mut("1").unwrap().trajectory.steps;
        steps[0].system_prompt.clear();
        steps[0].user_content.clear();
        let mut changed = steps[0].clone();
        changed.step_id = 2;
        changed.system_prompt = "system".into();
        changed.user_content = "task".into();
        changed.assistant_content.content = "second".into();
        changed.assistant_content.tool_calls.clear();
        changed.tools.clear();
        changed.observation.clear();
        let mut again = changed.clone();
        again.step_id = 3;
        again.assistant_content.content = "third".into();
        let mut overlay = changed.clone();
        overlay.step_id = 4;
        overlay.user_content = "later".into();
        overlay.assistant_content.content = "fourth".into();
        steps.push(changed);
        steps.push(again);
        steps.push(overlay);

        let story = actf_to_storyline(&document).unwrap();
        assert_eq!(
            story.prompt.as_ref().map(StorylinePrompt::pair),
            Some(("system".into(), "task".into()))
        );
        assert_eq!(
            story.turns[0].prompt,
            Some(StorylinePrompt::explicit_clear())
        );
        assert_eq!(story.turns[1].prompt, None);
        assert_eq!(story.turns[2].prompt, None);
        assert_eq!(
            story.turns[3].prompt.as_ref().map(StorylinePrompt::pair),
            Some(("system".into(), "later".into()))
        );
        assert_eq!(story.turns[3].message, json!("fourth"));
        assert!(
            !story
                .unknown_fields
                .sources
                .get("actf")
                .map(|source| source.fields.keys().any(|key| {
                    key.ends_with("/system_prompt") || key.ends_with("/user_content")
                }))
                .unwrap_or(false)
        );

        let restored = storyline_to_actf(&story).unwrap();
        let restored_steps = &restored.attempts["1"].trajectory.steps;
        assert_eq!(restored_steps[0].system_prompt, "");
        assert_eq!(restored_steps[0].user_content, "");
        assert_eq!(restored_steps[1].system_prompt, "system");
        assert_eq!(restored_steps[1].user_content, "task");
        assert_eq!(restored_steps[2].system_prompt, "system");
        assert_eq!(restored_steps[2].user_content, "task");
        assert_eq!(restored_steps[3].system_prompt, "system");
        assert_eq!(restored_steps[3].user_content, "later");
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
                kind: None,
                response: None,
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
            env: None,
            prompt: None,
            finished_at: None,
        });

        let document = storyline_to_actf(&story).unwrap();
        let step = &document.attempts["1"].trajectory.steps[0];
        assert_eq!(step.observation[0].kind, "tool_result");
        assert_eq!(step.observation[0].extra["tool_use_id"], "call-1");
        assert_eq!(step.metric.prompt_tokens_len, 0);
        assert_eq!(step.metric.completion_tokens_len, 0);
        assert_eq!(step.metric.extra["reward"], 1.0);
    }

    #[test]
    fn actf_observations_without_ids_are_correlated_by_tool_name_and_order() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        let step = &mut value["attempts"]["1"]["trajectory"]["steps"][0];
        step["tools"][0].as_object_mut().unwrap().remove("id");
        step["assistant_content"]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove("id");
        step["observation"] = json!([
            {"role": "tool", "tool_names": ["Bash"], "text": "ok"},
            {"role": "runtime", "tool_names": [], "text": ""}
        ]);

        let story = actf_to_storyline(&serde_json::from_value(value).unwrap()).unwrap();
        let turn = &story.turns[0];
        let call = &turn.tool_calls.as_ref().unwrap()[0];
        assert_eq!(call.tool_call_id, "step-1-tool-0");
        assert_eq!(
            turn.observation.as_ref().unwrap()["results"][0]["source_call_id"],
            "step-1-tool-0"
        );
        assert_eq!(
            turn.observation.as_ref().unwrap()["results"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn actf_step_metrics_preserve_custom_fields_and_fractional_values() {
        let mut value: Value = serde_json::from_str(FIXTURE).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["metric"] = json!({
            "prompt_tokens_len": 11,
            "completion_tokens_len": 7,
            "llm_infer_ms": 3.75,
            "env_action_ms": 4.25,
            "stop_reason": "tool_use",
            "reward": 0.125,
            "provider_metrics": {"cache_hit": true, "sampled": 0.5}
        });
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        let story = actf_to_storyline(&document).unwrap();
        let metrics = story.turns[0].metrics.as_ref().unwrap();
        assert_eq!(metrics["llm_infer_ms"], json!(3.75));
        assert_eq!(metrics["env_action_ms"], json!(4.25));
        assert_eq!(metrics["provider_metrics"]["cache_hit"], json!(true));
        assert_eq!(story.turns[0].latency_ms, Some(3));

        let restored = storyline_to_actf(&story).unwrap();
        assert_eq!(
            restored.attempts["1"].trajectory.steps[0].metric,
            document.attempts["1"].trajectory.steps[0].metric
        );
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn actf_lance_import_and_restore_preserves_semantics() {
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

        let restored_document = storyline_to_actf(&restored_story).unwrap();
        assert_eq!(restored_document.task_id, document.task_id);
        assert_eq!(restored_document.attempts.len(), document.attempts.len());
        assert_eq!(
            restored_document.attempts["1"].trajectory.steps[0].assistant_content,
            document.attempts["1"].trajectory.steps[0].assistant_content
        );
        assert_eq!(
            restored_story.turns[0]
                .timestamp
                .as_ref()
                .map(|value| value.timestamp_nanos()),
            story.turns[0]
                .timestamp
                .as_ref()
                .map(|value| value.timestamp_nanos())
        );
        assert_eq!(
            restored_story
                .finished_at
                .as_ref()
                .map(|value| value.timestamp_nanos()),
            story
                .finished_at
                .as_ref()
                .map(|value| value.timestamp_nanos())
        );
    }

    #[cfg(feature = "lance-store")]
    #[tokio::test]
    async fn fractional_space_timestamp_is_normalized_through_lance() {
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

        let restored_document = storyline_to_actf(&restored).unwrap();
        let original_timestamp =
            StorylineTimestamp::from_rfc3339("2026-01-01T00:00:00.123456Z").unwrap();
        let restored_timestamp = StorylineTimestamp::from_rfc3339(
            &restored_document.attempts["1"].trajectory.steps[0].started_at,
        )
        .unwrap();
        assert_eq!(
            restored_timestamp.timestamp_nanos(),
            original_timestamp.timestamp_nanos()
        );
    }
}
