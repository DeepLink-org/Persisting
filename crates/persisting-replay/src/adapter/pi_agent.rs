use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::{check_boundary, prepared_outcome, run_sdk_bridge, RunContext};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, AgentKind, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan, ToolBatch,
    ToolCall,
};

const SUPPORTED_TOOLS: &[&str] = &["read", "bash", "edit", "write"];

pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_pi_plan(request).map(AdapterPlan::PiAgent)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let boundary = plan.batches.last().unwrap().native["event_index"]
        .as_u64()
        .expect("validated Pi batch event index") as usize;
    let events = plan
        .native
        .as_array()
        .expect("Pi replay plan stores an event array");
    let path = context.output_dir.join("native/prepared-prefix.jsonl");
    write_jsonl(&path, &events[..=boundary])?;
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
    run_sdk_bridge(plan, context, journal, AgentKind::PiAgent)
}

fn build_pi_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let events = parse_jsonl_events(&raw)?;
    let last_turn_end = events
        .iter()
        .rposition(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"));
    let mut batches = Vec::new();
    for (event_index, event) in events.iter().enumerate() {
        if event.get("type").and_then(Value::as_str) != Some("turn_end") {
            continue;
        }
        let Some(message) = event.get("message") else {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has no message"
            )));
        };
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] message is not an assistant message"
            )));
        }
        let native_calls = pi_calls(message, event_index)?;
        if native_calls.is_empty() {
            continue;
        }
        let results = event
            .get("toolResults")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ReplayError::trajectory(format!(
                    "Pi turn_end event[{event_index}] has no toolResults array"
                ))
            })?;
        if results.len() != native_calls.len() {
            // A killed source run can end with an incomplete final batch. It is
            // not selectable, but earlier complete batches remain replayable.
            if Some(event_index) == last_turn_end {
                break;
            }
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has {} tool calls but {} results",
                native_calls.len(),
                results.len()
            )));
        }
        let mut result_ids = BTreeSet::new();
        for result in results {
            let result_id = result
                .get("toolCallId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ReplayError::trajectory(format!(
                        "Pi turn_end event[{event_index}] has a result without toolCallId"
                    ))
                })?;
            if !result_ids.insert(result_id) {
                return Err(ReplayError::trajectory(format!(
                    "Pi turn_end event[{event_index}] has duplicate result id {result_id:?}"
                )));
            }
        }
        let mut calls = Vec::with_capacity(native_calls.len());
        for (ordinal, native) in native_calls.into_iter().enumerate() {
            let call_id = native["id"]
                .as_str()
                .expect("validated Pi tool call id")
                .to_owned();
            let result = results
                .iter()
                .find(|result| {
                    result.get("toolCallId").and_then(Value::as_str) == Some(call_id.as_str())
                })
                .ok_or_else(|| {
                    ReplayError::trajectory(format!(
                        "Pi turn_end event[{event_index}] has no result for tool call {call_id:?}"
                    ))
                })?;
            if result.get("toolName").and_then(Value::as_str)
                != native.get("name").and_then(Value::as_str)
            {
                return Err(ReplayError::trajectory(format!(
                    "Pi result for {call_id:?} does not match its tool name"
                )));
            }
            calls.push(ToolCall {
                ordinal: ordinal + 1,
                call_id,
                name: native["name"]
                    .as_str()
                    .expect("validated Pi tool name")
                    .to_owned(),
                arguments: native["arguments"].clone(),
                original_observation: result.get("content").cloned().unwrap_or(Value::Null),
                original_is_error: result
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                native,
            });
        }
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("events:{event_index}"),
            tool_calls: calls,
            assistant_text: pi_text(message),
            native: json!({"event_index": event_index}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    let boundary_event_index = batches[request.after_step - 1].native["event_index"]
        .as_u64()
        .expect("validated Pi batch event index") as usize;
    let prefix_model_turns = events[..=boundary_event_index]
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
        .count();
    let original_next_action = events[boundary_event_index + 1..]
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
        .and_then(|event| event.get("message"))
        .map(pi_action_signature);
    batches.truncate(request.after_step);
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        batches,
        prefix_model_turns,
        native: Value::Array(events),
        original_next_action,
    })
}

fn parse_jsonl_events(raw: &[u8]) -> Result<Vec<Value>, ReplayError> {
    let source = std::str::from_utf8(raw).map_err(|error| {
        ReplayError::trajectory(format!("Pi event JSONL is not UTF-8: {error}"))
    })?;
    let physical = source.split('\n').collect::<Vec<_>>();
    let mut events = Vec::new();
    for (index, line) in physical.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(Value::Object(event)) => events.push(Value::Object(event)),
            Ok(_) => {
                return Err(ReplayError::trajectory(format!(
                    "Pi event JSONL line {} must contain an object",
                    index + 1
                )))
            }
            Err(_) if index + 1 == physical.len() && !source.ends_with('\n') => {
                // The source process may be killed while writing its final line.
            }
            Err(error) => {
                return Err(ReplayError::trajectory(format!(
                    "invalid Pi event JSONL line {}: {error}",
                    index + 1
                )))
            }
        }
    }
    if !events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("message_end")
            && event.pointer("/message/role").and_then(Value::as_str) == Some("user")
    }) {
        return Err(ReplayError::trajectory(
            "Pi event trajectory has no native user message",
        ));
    }
    Ok(events)
}

fn pi_calls(message: &Value, event_index: usize) -> Result<Vec<Value>, ReplayError> {
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] assistant content must be an array"
            ))
        })?;
    let mut calls = Vec::new();
    let mut call_ids = BTreeSet::new();
    for (part_index, part) in content.iter().enumerate() {
        if part.get("type").and_then(Value::as_str) != Some("toolCall") {
            continue;
        }
        let id = part.get("id").and_then(Value::as_str).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] toolCall[{part_index}] has no id"
            ))
        })?;
        let name = part.get("name").and_then(Value::as_str).ok_or_else(|| {
            ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] toolCall[{part_index}] has no name"
            ))
        })?;
        if !call_ids.insert(id) {
            return Err(ReplayError::trajectory(format!(
                "Pi turn_end event[{event_index}] has duplicate tool call id {id:?}"
            )));
        }
        if !SUPPORTED_TOOLS.contains(&name) {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("Pi Replay profile does not support tool {name:?}"),
            ));
        }
        let arguments = part.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !arguments.is_object() {
            return Err(ReplayError::trajectory(format!(
                "Pi tool call {id:?} arguments must be an object"
            )));
        }
        calls.push(json!({"id": id, "name": name, "arguments": arguments}));
    }
    Ok(calls)
}

fn pi_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn pi_action_signature(message: &Value) -> Value {
    let mut reasoning = Vec::new();
    let mut tools = Vec::new();
    for part in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match part.get("type").and_then(Value::as_str) {
            Some("thinking") => {
                if let Some(value) = part.get("thinking").and_then(Value::as_str) {
                    reasoning.push(value);
                }
            }
            Some("toolCall") => tools.push(json!({
                "name": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                "arguments": part.get("arguments").cloned().unwrap_or_else(|| json!({})),
            })),
            _ => {}
        }
    }
    json!({
        "text": pi_text(message),
        "reasoning": reasoning.join("\n\n"),
        "tools": tools,
    })
}

fn write_jsonl(path: &std::path::Path, values: &[Value]) -> Result<(), ReplayError> {
    let mut rendered = Vec::new();
    for value in values {
        serde_json::to_writer(&mut rendered, value)
            .replay_context(ReplayErrorKind::Executor, "serialize Pi event JSONL")?;
        rendered.push(b'\n');
    }
    atomic_write(path, &rendered)
}

#[cfg(test)]
mod tests {
    use super::{parse_jsonl_events, pi_action_signature};
    use serde_json::json;

    #[test]
    fn pi_signature_separates_text_reasoning_and_tools() {
        let signature = pi_action_signature(&json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "inspect"},
                {"type": "text", "text": "I will inspect."},
                {"type": "toolCall", "id": "call-1", "name": "read", "arguments": {"path": "a"}}
            ]
        }));
        assert_eq!(signature["text"], "I will inspect.");
        assert_eq!(signature["reasoning"], "inspect");
        assert_eq!(signature["tools"][0]["name"], "read");
    }

    #[test]
    fn pi_jsonl_tolerates_only_a_truncated_final_line() {
        let raw = b"{\"type\":\"message_end\",\"message\":{\"role\":\"user\"}}\n{\"type\":";
        assert_eq!(parse_jsonl_events(raw).unwrap().len(), 1);
        let invalid = b"{\"type\":\"message_end\",\"message\":{\"role\":\"user\"}}\nnot-json\n";
        assert!(parse_jsonl_events(invalid).is_err());
    }
}
