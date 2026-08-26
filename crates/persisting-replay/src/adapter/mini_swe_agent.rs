use serde_json::{json, Value};

use super::{check_boundary, prepared_outcome, run_sdk_bridge, RunContext};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, AgentKind, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan, ToolBatch,
    ToolCall,
};

pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_mini_plan(request).map(AdapterPlan::MiniSweAgent)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_mini(plan, context, journal)
}

fn mini_reasoning(message: &Value) -> &str {
    message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .pointer("/extra/response/choices/0/message/reasoning_content")
                .and_then(Value::as_str)
        })
        .unwrap_or_default()
}

fn mini_batch_signature(batch: &ToolBatch, message: &Value) -> Value {
    json!({
        "text": batch.assistant_text.as_str(),
        "reasoning": mini_reasoning(message),
        "tools": batch.tool_calls.iter().map(|call| json!({
            "name": call.name.as_str(),
            "arguments": &call.arguments,
        })).collect::<Vec<_>>(),
    })
}

fn build_mini_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let value: Value = serde_json::from_slice(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "invalid mini-swe-agent trajectory JSON",
    )?;
    if value.get("trajectory_format").and_then(Value::as_str) != Some("mini-swe-agent-1.1") {
        return Err(ReplayError::trajectory(
            "mini-swe-agent trajectory_format must be mini-swe-agent-1.1",
        ));
    }
    if value
        .get("info")
        .and_then(|info| info.get("mini_version"))
        .and_then(Value::as_str)
        != Some("2.4.6")
    {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            "mini-swe-agent trajectory requires exact version 2.4.6",
        ));
    }
    let messages = value
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ReplayError::trajectory("mini-swe-agent messages must be an array"))?;
    let mut batches = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        let native_calls = mini_calls(message, message_index)?;
        if native_calls.is_empty() {
            continue;
        }
        let mut observations = Vec::new();
        for candidate in messages.iter().skip(message_index + 1) {
            if !mini_calls(candidate, message_index + 1 + observations.len())?.is_empty() {
                break;
            }
            if matches!(
                candidate.get("role").and_then(Value::as_str),
                Some("tool" | "user")
            ) || candidate.get("type").and_then(Value::as_str) == Some("function_call_output")
            {
                observations.push(candidate);
                if observations.len() == native_calls.len() {
                    break;
                }
            }
        }
        if observations.len() != native_calls.len() {
            break;
        }
        let batch_is_in_prefix = batches.len() < request.after_step;
        let calls = native_calls
            .into_iter()
            .zip(observations)
            .enumerate()
            .map(|(index, (native, observation))| {
                let command = native["arguments"]["command"].as_str().unwrap_or_default();
                if mini_submission_in_prefix(batch_is_in_prefix, command) {
                    return Err(ReplayError::new(
                        ReplayErrorKind::UnsupportedVersion,
                        "mini-swe-agent submission cannot appear inside a replay prefix",
                    ));
                }
                let return_code = observation
                    .get("extra")
                    .and_then(|extra| extra.get("returncode"))
                    .and_then(Value::as_i64);
                Ok(ToolCall {
                    ordinal: index + 1,
                    call_id: native["id"].as_str().unwrap().to_owned(),
                    name: "bash".into(),
                    arguments: native["arguments"].clone(),
                    original_observation: mini_observation(observation),
                    original_is_error: return_code.is_some_and(|code| code != 0),
                    native,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("messages:{message_index}"),
            tool_calls: calls,
            assistant_text: message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            native: json!({"message_index": message_index}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    let original_next_action = if let Some(batch) = batches.get(request.after_step) {
        let message_index = batch.native["message_index"].as_u64().ok_or_else(|| {
            ReplayError::trajectory("mini-swe-agent next action lost message_index")
        })? as usize;
        let message = messages.get(message_index).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "mini-swe-agent next action message index {message_index} is out of bounds"
            ))
        })?;
        Some(mini_batch_signature(batch, message))
    } else {
        None
    };
    batches.truncate(request.after_step);
    let boundary_message_index = batches.last().unwrap().native["message_index"]
        .as_u64()
        .ok_or_else(|| ReplayError::trajectory("mini-swe-agent batch lost message_index"))?
        as usize;
    let prefix_model_turns = value["messages"]
        .as_array()
        .ok_or_else(|| ReplayError::trajectory("mini-swe-agent messages must be an array"))?
        .iter()
        .take(boundary_message_index + 1)
        .filter(|message| {
            message
                .get("extra")
                .and_then(|extra| extra.get("response"))
                .is_some_and(Value::is_object)
        })
        .count();
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        prefix_model_turns,
        batches,
        native: value,
        original_next_action,
    })
}

fn mini_submission_in_prefix(batch_is_in_prefix: bool, command: &str) -> bool {
    batch_is_in_prefix
        && command
            .trim_start()
            .starts_with("echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT")
}

fn mini_calls(message: &Value, message_index: usize) -> Result<Vec<Value>, ReplayError> {
    if let Some(actions) = message
        .get("extra")
        .and_then(|extra| extra.get("actions"))
        .and_then(Value::as_array)
    {
        let native_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let command = action
                    .get("command")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ReplayError::trajectory(format!(
                            "mini-swe-agent message[{message_index}] has an invalid native action"
                        ))
                    })?;
                let call_id = action
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        native_calls
                            .get(index)
                            .and_then(|call| call.get("id"))
                            .and_then(Value::as_str)
                    })
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("mini-{message_index}-{}", index + 1));
                Ok(json!({
                    "id": call_id,
                    "arguments": {"command": command},
                    "native": action,
                }))
            })
            .collect();
    }
    let mut result = Vec::new();
    for (index, call) in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ReplayError::trajectory(format!(
                    "mini-swe-agent message[{message_index}] has an invalid tool call"
                ))
            })?;
        if function.get("name").and_then(Value::as_str) != Some("bash") {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                "mini-swe-agent playback supports only native bash actions",
            ));
        }
        let arguments = match function.get("arguments") {
            Some(Value::String(raw)) => serde_json::from_str(raw).replay_context(
                ReplayErrorKind::Trajectory,
                "invalid mini-swe-agent tool arguments",
            )?,
            Some(value) => value.clone(),
            None => json!({}),
        };
        if arguments.get("command").and_then(Value::as_str).is_none() {
            return Err(ReplayError::trajectory(
                "mini-swe-agent bash action has no command",
            ));
        }
        result.push(json!({
            "id": call.get("id").and_then(Value::as_str)
                .map(str::to_owned).unwrap_or_else(|| format!("mini-{message_index}-{}", index + 1)),
            "arguments": arguments,
            "native": call,
        }));
    }
    Ok(result)
}

fn mini_observation(message: &Value) -> Value {
    message
        .get("extra")
        .and_then(|extra| extra.get("raw_output"))
        .cloned()
        .or_else(|| message.get("output").cloned())
        .or_else(|| message.get("content").cloned())
        .unwrap_or(Value::String(String::new()))
}

fn run_mini(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let boundary = plan.batches.last().unwrap().native["message_index"]
        .as_u64()
        .unwrap() as usize;
    let mut prepared = plan.native.clone();
    prepared["messages"] =
        Value::Array(plan.native["messages"].as_array().unwrap()[..=boundary].to_vec());
    let path = context.output_dir.join("native/prepared-prefix.json");
    atomic_write_json(&path, &prepared)?;
    journal.append(
        "session_rebuilt",
        [(
            "prepared_only".into(),
            json!(context.request.mode == ReplayMode::PrepareOnly),
        )],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(path, context.request));
    }
    run_sdk_bridge(plan, context, journal, AgentKind::MiniSweAgent)
}

#[cfg(test)]
mod tests {
    use super::mini_submission_in_prefix;

    #[test]
    fn mini_submit_is_rejected_only_inside_the_selected_prefix() {
        let command = "  echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT";
        assert!(mini_submission_in_prefix(true, command));
        assert!(!mini_submission_in_prefix(false, command));
        assert!(!mini_submission_in_prefix(true, "echo still-working"));
    }
}
