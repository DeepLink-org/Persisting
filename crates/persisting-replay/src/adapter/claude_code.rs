use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{
    agent_command, check_boundary, sanitized_environment, with_boundary_user_prompt_metadata,
    RunContext, MAX_TOOL_OUTPUT_BYTES,
};
use crate::claude_bridge::ClaudeBridgeHandle;
use crate::claude_resume::ResumeTransportManifest;
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write, atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, FreshObservation, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan,
    ToolBatch, ToolCall,
};
use crate::process::{run_process, ProcessSpec};

const FRESH_CLAUDE_TOOLS: &[&str] = &["Bash", "Edit", "Glob", "Grep", "MultiEdit", "Read", "Write"];
const STALE_CLAUDE_TOOLS: &[&str] = &[
    "Agent",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "TaskUpdate",
    "TodoWrite",
];

fn required_str<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str, ReplayError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ReplayError::trajectory(format!("{context} has no {field}")))
}

pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_claude_plan(request).map(AdapterPlan::ClaudeCode)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_claude(plan, context, journal)
}

fn claude_boundary_tool_use_ids(plan: &ReplayPlan) -> Vec<String> {
    let Some(last_batch) = plan.batches.last() else {
        return Vec::new();
    };
    let Some(events) = plan.native.get("events").and_then(Value::as_array) else {
        return last_batch
            .tool_calls
            .iter()
            .map(|call| call.call_id.clone())
            .collect();
    };
    let last_message_id = last_batch
        .native
        .get("assistant_index")
        .and_then(Value::as_u64)
        .and_then(|index| events.get(index as usize))
        .and_then(|event| event.get("message"))
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str);
    let mut grouped = plan
        .batches
        .iter()
        .rev()
        .take_while(|batch| {
            batch
                .native
                .get("assistant_index")
                .and_then(Value::as_u64)
                .and_then(|index| events.get(index as usize))
                .and_then(|event| event.get("message"))
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                == last_message_id
        })
        .collect::<Vec<_>>();
    grouped.reverse();
    grouped
        .into_iter()
        .flat_map(|batch| batch.tool_calls.iter().map(|call| call.call_id.clone()))
        .collect()
}

fn claude_canonical_messages(canonical: &str) -> Result<Vec<Value>, ReplayError> {
    let events = canonical
        .lines()
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Value>(line).replay_context(
                ReplayErrorKind::Trajectory,
                format!("invalid rebuilt Claude JSONL at line {}", index + 1),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (chain_indices, _) = claude_active_chain(&events)?;
    let source_prompt = chain_indices
        .iter()
        .map(|index| &events[*index])
        .find_map(|event| {
            if event.get("type").and_then(Value::as_str) != Some("user") {
                return None;
            }
            let content = event.get("message")?.get("content")?;
            if content.as_array().is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            }) {
                return None;
            }
            Some(claude_render_text(content))
        })
        .ok_or_else(|| ReplayError::trajectory("Claude canonical chain has no initial prompt"))?;
    let (turns, _) = parse_claude_turns(&events)?;
    let mut messages = vec![json!({"role": "user", "content": source_prompt})];
    let mut index = 0;
    while index < turns.len() {
        let turn = &turns[index];
        let batch = turn.batch.as_ref().ok_or_else(|| {
            ReplayError::trajectory("text-only Claude turn precedes the canonical replay boundary")
        })?;
        let message_id = batch
            .native
            .get("message_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ReplayError::trajectory("Claude canonical turn has no message ID"))?;
        let mut grouped = vec![(turn, batch)];
        while let Some(next) = turns.get(index + grouped.len()) {
            let Some(next_batch) = next.batch.as_ref() else {
                break;
            };
            if next_batch.native.get("message_id").and_then(Value::as_str) != Some(message_id) {
                break;
            }
            grouped.push((next, next_batch));
        }

        let mut assistant_blocks = Vec::new();
        let mut result_blocks = Vec::new();
        for (grouped_turn, grouped_batch) in &grouped {
            assistant_blocks.extend(claude_canonical_assistant_content(
                &events,
                grouped_turn,
                grouped_batch,
            )?);
            for call in &grouped_batch.tool_calls {
                let mut result = json!({
                    "type": "tool_result",
                    "tool_use_id": call.call_id,
                    "content": call.original_observation,
                });
                if call.original_is_error {
                    result["is_error"] = Value::Bool(true);
                }
                result_blocks.push(result);
            }
        }
        messages.push(json!({"role": "assistant", "content": assistant_blocks}));
        messages.push(json!({"role": "user", "content": result_blocks}));
        index += grouped.len();
    }
    if messages.len() < 3 {
        return Err(ReplayError::trajectory(
            "Claude canonical messages do not end in a replayed tool result",
        ));
    }
    Ok(messages)
}

fn claude_canonical_assistant_content(
    events: &[Value],
    turn: &ParsedClaudeTurn,
    batch: &ToolBatch,
) -> Result<Vec<Value>, ReplayError> {
    let indices = batch
        .native
        .get("assistant_indices")
        .and_then(Value::as_array)
        .ok_or_else(|| ReplayError::trajectory("Claude canonical turn lost assistant events"))?;
    let mut first_text = None;
    let mut first_thinking = None;
    let mut tools = BTreeMap::new();
    for index in indices {
        let index = index
            .as_u64()
            .ok_or_else(|| ReplayError::trajectory("Claude assistant index is not an integer"))?
            as usize;
        for raw in events
            .get(index)
            .and_then(|event| event.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match raw.get("type").and_then(Value::as_str) {
                Some("text") if first_text.is_none() => first_text = Some(raw.clone()),
                Some("thinking") if first_thinking.is_none() => first_thinking = Some(raw.clone()),
                Some("tool_use") => {
                    if let Some(call_id) = raw.get("id").and_then(Value::as_str) {
                        tools
                            .entry(call_id.to_owned())
                            .or_insert_with(|| raw.clone());
                    }
                }
                Some("server_tool_use") => {
                    return Err(ReplayError::new(
                        ReplayErrorKind::UnsupportedVersion,
                        "Claude server_tool_use is not replayable",
                    ));
                }
                _ => {}
            }
        }
    }
    let reasoning = turn
        .signature
        .get("reasoning")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = turn
        .signature
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut output = Vec::new();
    if first_thinking.is_some() || !reasoning.is_empty() {
        let mut block = first_thinking.unwrap_or_else(|| json!({"type": "thinking"}));
        block["thinking"] = Value::String(reasoning.to_owned());
        output.push(block);
    }
    if first_text.is_some() || !text.is_empty() {
        let mut block = first_text.unwrap_or_else(|| json!({"type": "text"}));
        block["text"] = Value::String(text.to_owned());
        if !text.trim().is_empty() {
            output.push(block);
        }
    }
    for call in &batch.tool_calls {
        output.push(tools.remove(&call.call_id).unwrap_or_else(|| {
            json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": call.arguments,
            })
        }));
    }
    if output.is_empty() {
        return Err(ReplayError::trajectory(
            "Claude canonical assistant turn has no model-visible content",
        ));
    }
    Ok(output)
}

fn claude_render_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_claude_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let text = std::str::from_utf8(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "Claude trajectory is not UTF-8",
    )?;
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            return Err(ReplayError::trajectory(format!(
                "blank Claude native line at {}",
                index + 1
            )));
        }
        let value: Value = serde_json::from_str(line).replay_context(
            ReplayErrorKind::Trajectory,
            format!("invalid Claude native JSONL at line {}", index + 1),
        )?;
        if !value.is_object() {
            return Err(ReplayError::trajectory(format!(
                "Claude native line {} is not an object",
                index + 1
            )));
        }
        events.push(value);
    }
    let versions: BTreeSet<_> = events
        .iter()
        .filter_map(|event| event.get("version").and_then(Value::as_str))
        .collect();
    if versions != BTreeSet::from(["2.1.220"]) {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!("Claude trajectory requires exact version 2.1.220; got {versions:?}"),
        ));
    }
    let session_ids: BTreeSet<_> = events
        .iter()
        .filter_map(|event| event.get("sessionId").and_then(Value::as_str))
        .collect();
    if session_ids.len() != 1 {
        return Err(ReplayError::trajectory(
            "Claude trajectory must contain exactly one session ID",
        ));
    }
    let session_id = session_ids.iter().next().unwrap().to_string();
    let (turns, active_chain_uuids) = parse_claude_turns(&events)?;
    let mut batches = Vec::new();
    for (turn_index, turn) in turns.iter().enumerate() {
        let Some(batch) = &turn.batch else {
            continue;
        };
        let mut batch = batch.clone();
        batch.ordinal = batches.len() + 1;
        batch.native["turn_index"] = json!(turn_index);
        batches.push(batch);
    }
    check_boundary(request.after_step, batches.len())?;
    for batch in batches.iter().take(request.after_step) {
        for call in &batch.tool_calls {
            let allow_stale =
                request.mode == ReplayMode::PrepareOnly || request.allow_stale_observations;
            validate_claude_tool_policy(call, allow_stale)?;
        }
    }
    let boundary_turn_index = batches[request.after_step - 1].native["turn_index"]
        .as_u64()
        .ok_or_else(|| ReplayError::trajectory("Claude batch lost its logical turn index"))?
        as usize;
    let original_next_action = turns
        .get(boundary_turn_index + 1)
        .map(|turn| turn.signature.clone());
    batches.truncate(request.after_step);
    let boundary_result_index = batches
        .last()
        .and_then(|batch| batch.native.get("terminal_result_index"))
        .and_then(Value::as_u64)
        .ok_or_else(|| ReplayError::trajectory("Claude boundary has no terminal result"))?
        as usize;
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        prefix_model_turns: batches.len(),
        batches,
        native: json!({
            "events": events,
            "session_id": session_id,
            "active_chain_uuids": active_chain_uuids,
            "boundary_result_index": boundary_result_index,
        }),
        original_next_action,
    })
}

#[derive(Debug)]
struct ParsedClaudeTurn {
    signature: Value,
    batch: Option<ToolBatch>,
}

fn parse_claude_turns(
    events: &[Value],
) -> Result<(Vec<ParsedClaudeTurn>, Vec<String>), ReplayError> {
    let (chain_indices, chain_uuids) = claude_active_chain(events)?;
    let chain_positions: BTreeMap<_, _> = chain_uuids
        .iter()
        .enumerate()
        .map(|(position, uuid)| (uuid.clone(), position))
        .collect();
    let result_index = claude_tool_result_index(events)?;
    let mut seen_call_ids = BTreeSet::new();
    let mut turns = Vec::new();
    let mut cursor = 0;

    while cursor < chain_indices.len() {
        let event_index = chain_indices[cursor];
        let event = &events[event_index];
        if event.get("type").and_then(Value::as_str) != Some("assistant") {
            cursor += 1;
            continue;
        }
        let message = event.get("message").unwrap_or(&Value::Null);
        if !matches!(
            message.get("role").and_then(Value::as_str),
            None | Some("assistant")
        ) {
            return Err(ReplayError::trajectory(format!(
                "Claude assistant event {event_index} has an invalid role"
            )));
        }
        let message_id = required_str(message, "id", "Claude assistant message")?.to_owned();
        let mut assistant_indices: Vec<usize> = Vec::new();
        while cursor < chain_indices.len() {
            let candidate_index = chain_indices[cursor];
            let candidate = &events[candidate_index];
            if candidate.get("type").and_then(Value::as_str) != Some("assistant") {
                if assistant_indices.is_empty() {
                    break;
                }
                let previous = &events[*assistant_indices.last().unwrap()];
                let response_incomplete = previous
                    .get("message")
                    .and_then(|value| value.get("stop_reason"))
                    .is_none_or(Value::is_null);
                if !response_incomplete {
                    break;
                }
                let mut lookahead = cursor;
                while lookahead < chain_indices.len()
                    && events[chain_indices[lookahead]]
                        .get("type")
                        .and_then(Value::as_str)
                        != Some("assistant")
                {
                    lookahead += 1;
                }
                if lookahead >= chain_indices.len()
                    || events[chain_indices[lookahead]]
                        .get("message")
                        .and_then(|value| value.get("id"))
                        .and_then(Value::as_str)
                        != Some(message_id.as_str())
                {
                    break;
                }
                cursor = lookahead;
                continue;
            }
            if candidate
                .get("message")
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                != Some(message_id.as_str())
            {
                break;
            }
            assistant_indices.push(candidate_index);
            cursor += 1;
        }

        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        let mut terminal_results: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let mut assistant_chain_positions = Vec::new();
        for &assistant_index in &assistant_indices {
            let assistant_event = &events[assistant_index];
            let assistant_uuid = required_str(assistant_event, "uuid", "Claude assistant event")?;
            if let Some(position) = chain_positions.get(assistant_uuid) {
                assistant_chain_positions.push(*position);
            }
            for block in assistant_event
                .get("message")
                .and_then(|value| value.get("content"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => merge_claude_streamed_text(
                        &mut reasoning,
                        block
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("text") => merge_claude_streamed_text(
                        &mut text,
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("server_tool_use") => {
                        return Err(ReplayError::new(
                            ReplayErrorKind::UnsupportedVersion,
                            "Claude server_tool_use is not replayable",
                        ));
                    }
                    Some("tool_use") => {
                        let call_id = required_str(block, "id", "Claude tool_use")?.to_owned();
                        if !seen_call_ids.insert(call_id.clone()) {
                            return Err(ReplayError::trajectory(format!(
                                "duplicate Claude tool_use id {call_id}"
                            )));
                        }
                        // Keep later malformed/unsupported calls in the parsed timeline so a
                        // valid earlier boundary remains replayable. Prefix validation above
                        // still rejects them if the user selects one for execution.
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let arguments = block
                            .get("input")
                            .filter(|input| input.is_object())
                            .cloned()
                            .ok_or_else(|| {
                                ReplayError::trajectory(format!(
                                    "Claude tool input for {call_id} is not an object"
                                ))
                            })?;
                        let (result_event_index, result) =
                            result_index.get(&call_id).ok_or_else(|| {
                                ReplayError::trajectory(format!(
                                    "active Claude tool_use {call_id} has no tool_result"
                                ))
                            })?;
                        let result_event = &events[*result_event_index];
                        let result_uuid =
                            required_str(result_event, "uuid", "Claude tool_result event")?;
                        if required_str(result_event, "parentUuid", "Claude tool_result event")?
                            != assistant_uuid
                            || required_str(
                                result_event,
                                "sourceToolAssistantUUID",
                                "Claude tool_result event",
                            )? != assistant_uuid
                        {
                            return Err(ReplayError::trajectory(format!(
                                "Claude tool_result {call_id} does not point to its tool_use event"
                            )));
                        }
                        if let Some(position) = chain_positions.get(result_uuid) {
                            terminal_results
                                .entry(result_uuid.to_owned())
                                .or_insert((*position, *result_event_index));
                        }
                        calls.push(ToolCall {
                            ordinal: calls.len() + 1,
                            call_id,
                            name,
                            arguments,
                            original_observation: result
                                .get("content")
                                .cloned()
                                .unwrap_or(Value::Null),
                            original_is_error: result
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            native: json!({
                                "assistant_index": assistant_index,
                                "assistant_uuid": assistant_uuid,
                                "result_index": result_event_index,
                                "result_uuid": result_uuid,
                            }),
                        });
                    }
                    _ => {}
                }
            }
        }

        let signature = json!({
            "text": text,
            "reasoning": reasoning,
            "tools": calls.iter().map(|call| json!({
                "name": call.name,
                "arguments": call.arguments,
            })).collect::<Vec<_>>(),
        });
        let batch = if calls.is_empty() {
            None
        } else {
            if terminal_results.is_empty() {
                return Err(ReplayError::trajectory(format!(
                    "Claude assistant turn {message_id} has no tool_result on its active chain"
                )));
            }
            let mut ordered_results = terminal_results.values().copied().collect::<Vec<_>>();
            ordered_results.sort_unstable();
            for (result_position, _) in ordered_results.iter().take(ordered_results.len() - 1) {
                if !assistant_chain_positions
                    .iter()
                    .any(|assistant_position| assistant_position > result_position)
                {
                    return Err(ReplayError::trajectory(format!(
                        "Claude assistant turn {message_id} has ambiguous terminal results"
                    )));
                }
            }
            let terminal_result_index = ordered_results.last().unwrap().1;
            Some(ToolBatch {
                ordinal: 0,
                native_locator: format!("event:{}", assistant_indices[0]),
                assistant_text: signature["text"].as_str().unwrap_or_default().to_owned(),
                tool_calls: calls,
                native: json!({
                    "assistant_index": assistant_indices[0],
                    "assistant_indices": assistant_indices,
                    "message_id": message_id,
                    "terminal_result_index": terminal_result_index,
                }),
            })
        };
        turns.push(ParsedClaudeTurn { signature, batch });
    }
    if turns.is_empty() {
        return Err(ReplayError::trajectory(
            "active Claude native chain contains no assistant turns",
        ));
    }
    Ok((turns, chain_uuids))
}

fn claude_active_chain(events: &[Value]) -> Result<(Vec<usize>, Vec<String>), ReplayError> {
    let mut events_by_uuid = BTreeMap::new();
    for (index, event) in events.iter().enumerate() {
        if !main_event(event) {
            continue;
        }
        let uuid = required_str(event, "uuid", "Claude main event")?.to_owned();
        if events_by_uuid.insert(uuid.clone(), index).is_some() {
            return Err(ReplayError::trajectory(format!(
                "duplicate Claude main-chain UUID {uuid}"
            )));
        }
    }
    let leaf_uuid = events
        .iter()
        .rev()
        .find(|event| {
            main_event(event)
                && matches!(
                    event.get("type").and_then(Value::as_str),
                    Some("assistant" | "user")
                )
        })
        .and_then(|event| event.get("uuid"))
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("Claude session has no active leaf"))?
        .to_owned();
    let mut reversed_indices = Vec::new();
    let mut reversed_uuids = Vec::new();
    let mut seen = BTreeSet::new();
    let mut cursor = Some(leaf_uuid);
    while let Some(uuid) = cursor {
        if !seen.insert(uuid.clone()) {
            return Err(ReplayError::trajectory(format!(
                "cycle in Claude UUID parent chain at {uuid}"
            )));
        }
        let index = *events_by_uuid.get(&uuid).ok_or_else(|| {
            ReplayError::trajectory(format!("Claude active chain lost parent {uuid}"))
        })?;
        reversed_indices.push(index);
        reversed_uuids.push(uuid);
        cursor = match events[index].get("parentUuid") {
            None | Some(Value::Null) => None,
            Some(Value::String(parent)) if !parent.is_empty() => Some(parent.clone()),
            _ => {
                return Err(ReplayError::trajectory(format!(
                    "Claude event {index} has an invalid parentUuid"
                )));
            }
        };
    }
    reversed_indices.reverse();
    reversed_uuids.reverse();
    Ok((reversed_indices, reversed_uuids))
}

fn claude_tool_result_index(
    events: &[Value],
) -> Result<BTreeMap<String, (usize, Value)>, ReplayError> {
    let mut results = BTreeMap::new();
    for (event_index, event) in events.iter().enumerate() {
        if !main_event(event) || event.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        if !matches!(
            event
                .get("message")
                .and_then(|message| message.get("role"))
                .and_then(Value::as_str),
            None | Some("user")
        ) {
            return Err(ReplayError::trajectory(format!(
                "Claude user event {event_index} has an invalid role"
            )));
        }
        for block in event
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let call_id = required_str(block, "tool_use_id", "Claude tool_result")?.to_owned();
            if results
                .insert(call_id.clone(), (event_index, block.clone()))
                .is_some()
            {
                return Err(ReplayError::trajectory(format!(
                    "duplicate Claude tool_result for {call_id}"
                )));
            }
        }
    }
    Ok(results)
}

fn merge_claude_streamed_text(existing: &mut String, incoming: &str) {
    if incoming.is_empty() || incoming == existing || existing.starts_with(incoming) {
        return;
    }
    if existing.is_empty() || incoming.starts_with(existing.as_str()) {
        *existing = incoming.to_owned();
    } else {
        existing.push_str("\n\n");
        existing.push_str(incoming);
    }
}

fn main_event(event: &Value) -> bool {
    event.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && event.get("uuid").and_then(Value::as_str).is_some()
}

fn run_claude(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let session_id = plan.native["session_id"]
        .as_str()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost its session ID"))?;
    if context.request.mode == ReplayMode::PrepareOnly {
        let replacements = plan
            .calls()
            .map(|call| {
                (
                    call.call_id.clone(),
                    FreshObservation {
                        call_id: call.call_id.clone(),
                        content: call.original_observation.clone(),
                        is_error: call.original_is_error,
                        return_code: None,
                        duration_ms: 0,
                        truncated: false,
                        metadata: BTreeMap::new(),
                    },
                )
            })
            .collect();
        let canonical = rebuild_claude(plan, &replacements)?;
        let prepared = context.output_dir.join("native/prepared-prefix.jsonl");
        atomic_write(&prepared, canonical.as_bytes())?;
        journal.append(
            "session_prepared",
            [("sha256".into(), json!(sha256(canonical.as_bytes())))],
        )?;
        return Ok(ReplayOutcome {
            status: "prepared".into(),
            reconstructed_path: Some(prepared),
            continued_path: None,
            observations: Vec::new(),
            continued_steps: 0,
            metadata: with_boundary_user_prompt_metadata(
                json!({"native_session_id": session_id}),
                context.request,
                false,
            ),
        });
    }
    let mut replacements = BTreeMap::new();
    let mut observations = Vec::new();
    let mut comparisons = Vec::new();
    let historical_logs = context.output_dir.join("logs/historical-tools");
    fs::create_dir_all(&historical_logs).replay_context(
        ReplayErrorKind::Executor,
        "create Claude historical tool log directory",
    )?;
    for batch in &plan.batches {
        journal.append("batch_started", [("batch".into(), json!(batch.ordinal))])?;
        for call in &batch.tool_calls {
            journal.append(
                "tool_started",
                [
                    ("batch".into(), json!(batch.ordinal)),
                    ("call_id".into(), json!(call.call_id)),
                    ("tool".into(), json!(call.name)),
                ],
            )?;
            let bash_log = historical_logs.join(format!("bash-{}.log", call.ordinal));
            let fresh = execute_claude_tool_with_policy(
                call,
                &context.request.workspace,
                context.request.allow_stale_observations,
                Some(&bash_log),
            )?;
            replacements.insert(call.call_id.clone(), fresh.clone());
            comparisons.push(json!({
                "call_id": call.call_id,
                "tool": call.name,
                "exact": call.original_observation == fresh.content
                    && call.original_is_error == fresh.is_error
                    && !fresh.metadata.contains_key("opaque_source_observation"),
                "original_is_error": call.original_is_error,
                "replayed_is_error": fresh.is_error,
            }));
            journal.append(
                "tool_finished",
                [
                    ("batch".into(), json!(batch.ordinal)),
                    ("call_id".into(), json!(call.call_id)),
                    ("return_code".into(), json!(fresh.return_code)),
                    ("is_error".into(), json!(fresh.is_error)),
                    ("duration_ms".into(), json!(fresh.duration_ms)),
                ],
            )?;
            observations.push(fresh);
        }
        journal.append("batch_committed", [("batch".into(), json!(batch.ordinal))])?;
    }
    let canonical = rebuild_claude(plan, &replacements)?;
    let reconstructed = context.output_dir.join("native/reconstructed-prefix.jsonl");
    atomic_write(&reconstructed, canonical.as_bytes())?;
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )?;
    journal.append(
        "session_rebuilt",
        [("sha256".into(), json!(sha256(canonical.as_bytes())))],
    )?;
    if context.request.mode == ReplayMode::ReplayOnly {
        return Ok(ReplayOutcome {
            status: "replayed".into(),
            reconstructed_path: Some(reconstructed),
            continued_path: None,
            observations,
            continued_steps: 0,
            metadata: with_boundary_user_prompt_metadata(
                json!({"native_session_id": session_id}),
                context.request,
                false,
            ),
        });
    }
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("Claude continuation has no launch spec"))?;
    if let Some(max_steps) = context.request.max_steps {
        if max_steps <= plan.prefix_model_turns {
            return Err(ReplayError::continuation(
                "max-steps is exhausted by the replay prefix",
            ));
        }
    }
    let remaining_turns = context
        .request
        .max_steps
        .map(|max_steps| max_steps - plan.prefix_model_turns);
    let config_dir = context.state_dir.join("claude-config");
    let project_key: String = context
        .request
        .workspace
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let native_path = config_dir
        .join("projects")
        .join(project_key)
        .join(format!("{session_id}.jsonl"));
    atomic_write(&native_path, canonical.as_bytes())?;
    let canonical_messages = claude_canonical_messages(&canonical)?;
    let manifest = ResumeTransportManifest::create(
        session_id,
        claude_boundary_tool_use_ids(plan),
        canonical_messages,
        context.nonce.to_owned(),
    )
    .map_err(|error| {
        ReplayError::trajectory(format!(
            "construct Claude Resume Transport manifest: {error}"
        ))
    })?;
    let bridge = ClaudeBridgeHandle::start(
        manifest,
        context.session_id,
        context.request.disable_thinking,
        context.request.boundary_user_prompt(),
    )?;
    journal.append("continuation_started", std::iter::empty())?;
    let mut command = agent_command(&launch.entrypoint, context);
    for (name, value) in bridge.child_environment() {
        command.env(name, value);
    }
    command
        .args(["--verbose", "--output-format=stream-json", "--resume"])
        .arg(session_id);
    if let Some(remaining_turns) = remaining_turns {
        command.args(["--max-turns", &remaining_turns.to_string()]);
    }
    if !context.request.disallowed_tools.is_empty() {
        command.args([
            "--disallowedTools",
            &context.request.disallowed_tools.join(","),
        ]);
    }
    command.args(["--permission-mode", "bypassPermissions", "--print"]);
    command.env("CLAUDE_CONFIG_DIR", &config_dir);
    let log = context.output_dir.join("logs/claude-code.jsonl");
    let output = run_process(ProcessSpec {
        command,
        stdin: Some(context.nonce.as_bytes().to_vec()),
        timeout: Duration::from_secs(24 * 60 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log.clone(),
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    let process_error = if output.timed_out
        || (!output.status.success()
            && !expected_claude_max_turn_exit(&output.stdout_tail, remaining_turns))
    {
        let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
        if !output.stderr_tail.is_empty() {
            rendered.push('\n');
            rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
        }
        Some(ReplayError::classify_continuation(
            format!(
                "Claude continuation exited {}; see {}",
                output.status,
                log.display()
            ),
            &rendered,
        ))
    } else {
        None
    };
    let bridge_result = bridge.finish();
    if let Some(mut process_error) = process_error {
        if let Err(bridge_error) = bridge_result {
            process_error.message = format!(
                "{}; SandboxReplay bridge shutdown/validation also failed: {}",
                process_error.message, bridge_error
            );
        }
        return Err(process_error);
    }
    let validated_model_requests = bridge_result?;
    let prompt_injected =
        context.request.boundary_user_prompt().is_some() && validated_model_requests > 0;
    let raw_continued = String::from_utf8(read_regular_file(&native_path)?).replay_context(
        ReplayErrorKind::Continuation,
        "continued Claude session is not UTF-8",
    )?;
    let (cleaned, continued_steps) =
        clean_claude_continuation(plan, context.nonce, &raw_continued)?;
    let continued = context.output_dir.join("native/continued-session.jsonl");
    atomic_write(&continued, cleaned.as_bytes())?;
    journal.append(
        "continuation_finished",
        [
            ("return_code".into(), json!(output.status.code())),
            ("continued_steps".into(), json!(continued_steps)),
            (
                "validated_model_requests".into(),
                json!(validated_model_requests),
            ),
            (
                "boundary_user_prompt_injected".into(),
                json!(prompt_injected),
            ),
        ],
    )?;
    Ok(ReplayOutcome {
        status: "completed".into(),
        reconstructed_path: Some(reconstructed),
        continued_path: Some(continued),
        observations,
        continued_steps,
        metadata: with_boundary_user_prompt_metadata(
            json!({
                "native_session_id": session_id,
                "validated_model_requests": validated_model_requests,
                "model_transport": "sandbox-replay-claude-bridge",
            }),
            context.request,
            prompt_injected,
        ),
    })
}

fn validate_claude_tool_policy(
    call: &ToolCall,
    allow_stale_observations: bool,
) -> Result<(), ReplayError> {
    let unsupported = || {
        ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!(
                "unsupported Claude replay tool call {}({}) inside the selected prefix",
                call.name, call.call_id
            ),
        )
    };
    if call.name == "Find"
        || (!FRESH_CLAUDE_TOOLS.contains(&call.name.as_str())
            && !STALE_CLAUDE_TOOLS.contains(&call.name.as_str()))
        || (call.name == "Bash"
            && call
                .arguments
                .get("run_in_background")
                .and_then(Value::as_bool)
                == Some(true))
        || (call.name == "Agent"
            && call.arguments.get("subagent_type").and_then(Value::as_str) != Some("Explore"))
    {
        return Err(unsupported());
    }
    let requires_stale = STALE_CLAUDE_TOOLS.contains(&call.name.as_str())
        || (call.original_is_error && claude_arguments_are_invalid(call));
    if requires_stale && !allow_stale_observations {
        return Err(ReplayError::trajectory(format!(
            "Claude tool call {}({}) can only reuse its source observation; pass --allow-stale-observations to opt into degraded replay",
            call.name, call.call_id
        )));
    }
    Ok(())
}

fn execute_claude_tool_with_policy(
    call: &ToolCall,
    workspace: &Path,
    allow_stale_observations: bool,
    bash_log: Option<&Path>,
) -> Result<FreshObservation, ReplayError> {
    validate_claude_tool_policy(call, allow_stale_observations)?;
    let started = Instant::now();
    if call.original_is_error && claude_arguments_are_invalid(call) {
        return replay_original_observation(call, started);
    }
    let (content, is_error, return_code) = match call.name.as_str() {
        "Agent" => {
            return replay_original_observation(call, started);
        }
        "TaskOutput" => {
            return replay_original_observation(call, started);
        }
        "Bash" => {
            let command = call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let timeout = call
                .arguments
                .get("timeout")
                .and_then(Value::as_u64)
                .map(|milliseconds| Duration::from_millis(milliseconds.clamp(1_000, 600_000)))
                .unwrap_or(Duration::from_secs(120));
            let log = bash_log.ok_or_else(|| {
                ReplayError::new(
                    ReplayErrorKind::Internal,
                    "Claude Bash replay requires a process log path",
                )
            })?;
            let (content, is_error, return_code, truncated) =
                run_bash(command, workspace, timeout, log)?;
            return observation_with_truncation(
                call,
                content,
                is_error,
                return_code,
                started,
                truncated,
            );
        }
        "Read" => {
            let path = tool_path(&call.arguments, workspace, true)?;
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    return observation(
                        call,
                        format!("Read failed for {}: {error}", path.display()),
                        true,
                        Some(1),
                        started,
                    );
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            let offset = call
                .arguments
                .get("offset")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            let limit = call
                .arguments
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX) as usize;
            let value = text
                .lines()
                .skip(offset - 1)
                .take(limit)
                .collect::<Vec<_>>()
                .join("\n");
            (value, false, Some(0))
        }
        "Write" => {
            let path = tool_path(&call.arguments, workspace, true)?;
            let content = call
                .arguments
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            fs::write(&path, content).replay_context(
                ReplayErrorKind::Executor,
                format!("write replay tool target {}", path.display()),
            )?;
            (
                format!("Wrote {} bytes to {}", content.len(), path.display()),
                false,
                Some(0),
            )
        }
        "Edit" => edit_tool(&call.arguments, workspace)?,
        "MultiEdit" => {
            let mut result = String::new();
            for edit in call
                .arguments
                .get("edits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let mut arguments = edit.clone();
                if arguments.get("file_path").is_none() {
                    arguments["file_path"] = call
                        .arguments
                        .get("file_path")
                        .cloned()
                        .unwrap_or(Value::Null);
                }
                let (message, is_error, code) = edit_tool(&arguments, workspace)?;
                if is_error {
                    return observation(call, message, true, code, started);
                }
                result.push_str(&message);
                result.push('\n');
            }
            (result.trim_end().to_owned(), false, Some(0))
        }
        "Glob" => {
            let pattern = call
                .arguments
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or("*");
            let root = call
                .arguments
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            let root = confined_path(workspace, &root, false)?;
            let mut matches = Vec::new();
            walk_files(&root, &mut |path| {
                let relative = path.strip_prefix(&root).unwrap_or(path).to_string_lossy();
                if wildcard_match(pattern, &relative) {
                    matches.push(path.display().to_string());
                }
            })?;
            (matches.join("\n"), false, Some(0))
        }
        "Grep" => {
            let Some(needle) = call
                .arguments
                .get("pattern")
                .or_else(|| call.arguments.get("search"))
                .and_then(Value::as_str)
                .filter(|needle| !needle.is_empty())
            else {
                return observation(
                    call,
                    "Grep failed: pattern/search is missing or empty".into(),
                    true,
                    Some(1),
                    started,
                );
            };
            let root = call
                .arguments
                .get("path")
                .or_else(|| call.arguments.get("files"))
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| workspace.to_path_buf());
            let root = confined_path(workspace, &root, false)?;
            let mut matches = Vec::new();
            walk_files(&root, &mut |path| {
                if let Ok(text) = fs::read_to_string(path) {
                    for (line, content) in text.lines().enumerate() {
                        if content.contains(needle) {
                            matches.push(format!("{}:{}:{}", path.display(), line + 1, content));
                        }
                    }
                }
            })?;
            (matches.join("\n"), false, Some(0))
        }
        "TaskCreate" | "TaskGet" | "TaskList" | "TaskUpdate" | "TodoWrite" => {
            return replay_original_observation(call, started);
        }
        other => {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("unsupported Claude replay tool {other}"),
            ));
        }
    };
    observation(call, content, is_error, return_code, started)
}

fn claude_arguments_are_invalid(call: &ToolCall) -> bool {
    let string = |name: &str| {
        call.arguments
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    };
    match call.name.as_str() {
        "Agent" => !(string("description") && string("prompt") && string("subagent_type")),
        "TaskOutput" => !string("task_id"),
        "Bash" => !string("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" => !(string("file_path") || string("path")),
        "Glob" => !string("pattern"),
        "Grep" => !(string("pattern") || string("search")),
        _ => false,
    }
}

fn replay_original_observation(
    call: &ToolCall,
    started: Instant,
) -> Result<FreshObservation, ReplayError> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "opaque_source_observation".into(),
        json!("stale_source_observation"),
    );
    metadata.insert(
        "degradation_reason".into(),
        json!("stale_source_observation"),
    );
    metadata.insert("source_call_id".into(), json!(call.call_id));
    Ok(FreshObservation {
        call_id: call.call_id.clone(),
        content: call.original_observation.clone(),
        is_error: call.original_is_error,
        return_code: Some(if call.original_is_error { 1 } else { 0 }),
        duration_ms: started.elapsed().as_millis(),
        truncated: false,
        metadata,
    })
}

fn observation(
    call: &ToolCall,
    content: String,
    is_error: bool,
    return_code: Option<i32>,
    started: Instant,
) -> Result<FreshObservation, ReplayError> {
    observation_with_truncation(call, content, is_error, return_code, started, false)
}

fn observation_with_truncation(
    call: &ToolCall,
    content: String,
    is_error: bool,
    return_code: Option<i32>,
    started: Instant,
    forced_truncated: bool,
) -> Result<FreshObservation, ReplayError> {
    let bytes = content.into_bytes();
    let truncated = forced_truncated || bytes.len() > MAX_TOOL_OUTPUT_BYTES;
    let content = if bytes.len() > MAX_TOOL_OUTPUT_BYTES {
        format!(
            "{}\n[output truncated by pvisor replay]",
            String::from_utf8_lossy(&bytes[..MAX_TOOL_OUTPUT_BYTES])
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(FreshObservation {
        call_id: call.call_id.clone(),
        content: Value::String(content),
        is_error,
        return_code,
        duration_ms: started.elapsed().as_millis(),
        truncated,
        metadata: BTreeMap::new(),
    })
}

fn run_bash(
    command: &str,
    workspace: &Path,
    timeout: Duration,
    log_path: &Path,
) -> Result<(String, bool, Option<i32>, bool), ReplayError> {
    let mut process = Command::new("/bin/bash");
    process.args(["-c", command]).current_dir(workspace);
    sanitized_environment(&mut process, true);
    let output = run_process(ProcessSpec {
        command: process,
        stdin: None,
        timeout,
        termination_grace: Duration::from_millis(250),
        pipe_grace: Duration::from_millis(100),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log_path.to_path_buf(),
    })?;
    let mut content = String::from_utf8_lossy(&output.stdout_tail).into_owned();
    if !output.stderr_tail.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&String::from_utf8_lossy(&output.stderr_tail));
    }
    if output.stdout_truncated || output.stderr_truncated {
        content
            .push_str("\n[output truncated by pvisor replay; full output is in the process log]");
    }
    if output.background_cleanup && !output.timed_out {
        content.push_str("\n[background descendants were terminated after the command exited]");
    }
    if output.timed_out {
        content = format!(
            "Command timed out after {} ms\n{}",
            timeout.as_millis(),
            content
        )
        .trim_end()
        .to_owned();
    }
    Ok((
        content,
        output.timed_out || output.background_cleanup || !output.status.success(),
        if output.timed_out {
            Some(124)
        } else {
            output.status.code()
        },
        output.stdout_truncated || output.stderr_truncated,
    ))
}

fn edit_tool(
    arguments: &Value,
    workspace: &Path,
) -> Result<(String, bool, Option<i32>), ReplayError> {
    let path = tool_path(arguments, workspace, false)?;
    let old = arguments
        .get("old_string")
        .or_else(|| arguments.get("old_str"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let new = arguments
        .get("new_string")
        .or_else(|| arguments.get("new_str"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = fs::read_to_string(&path).replay_context(
        ReplayErrorKind::Executor,
        format!("read edit target {}", path.display()),
    )?;
    let occurrences = source.matches(old).count();
    let replace_all = arguments
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if occurrences == 0 || (occurrences > 1 && !replace_all) {
        return Ok((
            format!("Edit rejected: old string occurs {occurrences} times"),
            true,
            Some(1),
        ));
    }
    let updated = if replace_all {
        source.replace(old, new)
    } else {
        source.replacen(old, new, 1)
    };
    fs::write(&path, updated).replay_context(
        ReplayErrorKind::Executor,
        format!("write edit target {}", path.display()),
    )?;
    Ok((format!("Updated {}", path.display()), false, Some(0)))
}

fn tool_path(
    arguments: &Value,
    workspace: &Path,
    allow_missing: bool,
) -> Result<PathBuf, ReplayError> {
    let raw = arguments
        .get("file_path")
        .or_else(|| arguments.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::new(ReplayErrorKind::Executor, "tool path is missing"))?;
    confined_path(workspace, Path::new(raw), allow_missing)
}

fn confined_path(
    workspace: &Path,
    path: &Path,
    allow_missing: bool,
) -> Result<PathBuf, ReplayError> {
    let workspace = canonicalize(workspace, ReplayErrorKind::Workspace, "workspace")?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    let resolved = if allow_missing && fs::symlink_metadata(&candidate).is_err() {
        let mut existing = candidate.as_path();
        let mut missing = Vec::new();
        while fs::symlink_metadata(existing).is_err() {
            missing.push(
                existing
                    .file_name()
                    .ok_or_else(|| {
                        ReplayError::new(ReplayErrorKind::Executor, "tool path has no file name")
                    })?
                    .to_os_string(),
            );
            existing = existing.parent().ok_or_else(|| {
                ReplayError::new(
                    ReplayErrorKind::Executor,
                    "tool path has no existing parent",
                )
            })?;
        }
        let mut resolved = canonicalize(existing, ReplayErrorKind::Executor, "tool path parent")?;
        for component in missing.iter().rev() {
            resolved.push(component);
        }
        resolved
    } else {
        canonicalize(&candidate, ReplayErrorKind::Executor, "tool path")?
    };
    if !resolved.starts_with(&workspace) {
        return Err(ReplayError::new(
            ReplayErrorKind::Executor,
            format!("tool path escapes workspace: {}", resolved.display()),
        ));
    }
    Ok(resolved)
}

fn walk_files(root: &Path, visit: &mut impl FnMut(&Path)) -> Result<(), ReplayError> {
    if root.is_file() {
        visit(root);
        return Ok(());
    }
    for entry in fs::read_dir(root).replay_context(
        ReplayErrorKind::Executor,
        format!("scan {}", root.display()),
    )? {
        let entry = entry.replay_context(ReplayErrorKind::Executor, "read directory entry")?;
        let file_type = entry
            .file_type()
            .replay_context(ReplayErrorKind::Executor, "read directory entry type")?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            walk_files(&entry.path(), visit)?;
        } else if file_type.is_file() {
            visit(&entry.path());
        }
    }
    Ok(())
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    fn matches(pattern: &[u8], value: &[u8]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some((&b'*', rest)) => {
                matches(rest, value) || (!value.is_empty() && matches(pattern, &value[1..]))
            }
            Some((&b'?', rest)) => !value.is_empty() && matches(rest, &value[1..]),
            Some((&character, rest)) => {
                value.first() == Some(&character) && matches(rest, &value[1..])
            }
        }
    }
    matches(pattern.as_bytes(), value.as_bytes())
}

fn rebuild_claude(
    plan: &ReplayPlan,
    replacements: &BTreeMap<String, FreshObservation>,
) -> Result<String, ReplayError> {
    let events = plan.native["events"]
        .as_array()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost native events"))?;
    let boundary = plan.native["boundary_result_index"]
        .as_u64()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost boundary"))?
        as usize;
    let boundary_uuid = events
        .get(boundary)
        .ok_or_else(|| ReplayError::trajectory("Claude boundary is outside the native session"))?
        .get("uuid")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("Claude boundary event has no UUID"))?;
    let active_chain = plan.native["active_chain_uuids"]
        .as_array()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost its active chain"))?;
    let boundary_chain_position = active_chain
        .iter()
        .position(|uuid| uuid.as_str() == Some(boundary_uuid))
        .ok_or_else(|| ReplayError::trajectory("Claude boundary is not on the active chain"))?;
    let mut allowed_uuids: BTreeSet<String> = active_chain
        .iter()
        .take(boundary_chain_position + 1)
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let selected: BTreeSet<_> = plan.calls().map(|call| call.call_id.as_str()).collect();
    let owner_by_call: BTreeMap<_, _> = plan
        .calls()
        .map(|call| {
            let owner = call
                .native
                .get("assistant_uuid")
                .and_then(Value::as_str)
                .ok_or_else(|| ReplayError::trajectory("Claude tool call lost its owner UUID"))?;
            let result_uuid = call
                .native
                .get("result_uuid")
                .and_then(Value::as_str)
                .ok_or_else(|| ReplayError::trajectory("Claude tool call lost its result UUID"))?;
            let result_index = call
                .native
                .get("result_index")
                .and_then(Value::as_u64)
                .ok_or_else(|| ReplayError::trajectory("Claude tool call lost its result index"))?
                as usize;
            if result_index > boundary {
                return Err(ReplayError::trajectory(
                    "Claude selected result appears after its logical boundary",
                ));
            }
            allowed_uuids.insert(result_uuid.to_owned());
            Ok((call.call_id.as_str(), owner))
        })
        .collect::<Result<_, ReplayError>>()?;
    let mut replaced = BTreeSet::new();
    let mut output_events = Vec::new();
    for event in events.iter().take(boundary + 1) {
        if !main_event(event) {
            continue;
        }
        let event_uuid = required_str(event, "uuid", "Claude canonical event")?;
        if !allowed_uuids.contains(event_uuid) {
            continue;
        }
        let mut updated = event.clone();
        let selected_call_ids = updated
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
            .filter(|call_id| selected.contains(call_id))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !selected_call_ids.is_empty() {
            let owners: BTreeSet<_> = selected_call_ids
                .iter()
                .filter_map(|call_id| owner_by_call.get(call_id.as_str()).copied())
                .collect();
            if owners.len() != 1 {
                return Err(ReplayError::trajectory(
                    "Claude tool_result event combines calls from different assistant events",
                ));
            }
            let owner = *owners.iter().next().unwrap();
            updated["parentUuid"] = Value::String(owner.to_owned());
            updated["sourceToolAssistantUUID"] = Value::String(owner.to_owned());
            if let Some(object) = updated.as_object_mut() {
                object.remove("toolUseResult");
            }
            if let Some(blocks) = updated
                .get_mut("message")
                .and_then(|message| message.get_mut("content"))
                .and_then(Value::as_array_mut)
            {
                for block in blocks {
                    let Some(call_id) = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .filter(|call_id| selected.contains(*call_id))
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    let replacement = replacements.get(&call_id).ok_or_else(|| {
                        ReplayError::trajectory(format!("fresh observation missing for {call_id}"))
                    })?;
                    block["content"] = replacement.content.clone();
                    if replacement.is_error {
                        block["is_error"] = Value::Bool(true);
                    } else if let Some(object) = block.as_object_mut() {
                        object.remove("is_error");
                    }
                    replaced.insert(call_id);
                }
            }
        }
        output_events.push(updated);
    }
    if replaced.len() != selected.len() {
        return Err(ReplayError::trajectory(
            "canonical Claude session did not replace every selected observation",
        ));
    }
    if output_events
        .last()
        .and_then(|event| event.get("uuid"))
        .and_then(Value::as_str)
        != Some(boundary_uuid)
    {
        return Err(ReplayError::trajectory(
            "canonical Claude session does not end at its logical boundary",
        ));
    }
    let mut output = output_events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .replay_context(ReplayErrorKind::Internal, "serialize Claude native event")?
        .join("\n");
    output.push('\n');
    Ok(output)
}

fn clean_claude_continuation(
    plan: &ReplayPlan,
    nonce: &str,
    raw: &str,
) -> Result<(String, usize), ReplayError> {
    let events: Vec<Value> = raw
        .lines()
        .map(|line| {
            serde_json::from_str(line).replay_context(
                ReplayErrorKind::Continuation,
                "parse resumed Claude native event",
            )
        })
        .collect::<Result<_, _>>()?;
    let boundary_uuid = plan.native["events"]
        .as_array()
        .and_then(|events| events.get(plan.native["boundary_result_index"].as_u64()? as usize))
        .and_then(|event| event.get("uuid"))
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("Claude boundary event has no UUID"))?
        .to_owned();
    let boundary_indexes: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            (event.get("uuid").and_then(Value::as_str) == Some(&boundary_uuid)).then_some(index)
        })
        .collect();
    if boundary_indexes.len() != 1 {
        return Err(ReplayError::continuation(
            "resumed Claude session must contain exactly one boundary observation",
        ));
    }
    let boundary_index = boundary_indexes[0];
    let envelope = events
        .get(boundary_index + 1..boundary_index + 6)
        .ok_or_else(|| ReplayError::continuation("Claude native resume envelope is incomplete"))?;
    let [enqueue, dequeue, continue_event, no_response_event, nonce_event] = envelope else {
        return Err(ReplayError::continuation(
            "Claude native resume envelope is incomplete",
        ));
    };
    let session_id = plan.native["session_id"]
        .as_str()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost its session ID"))?;
    if enqueue.get("type").and_then(Value::as_str) != Some("queue-operation")
        || enqueue.get("operation").and_then(Value::as_str) != Some("enqueue")
        || enqueue.get("content").and_then(Value::as_str) != Some(nonce)
        || enqueue.get("sessionId").and_then(Value::as_str) != Some(session_id)
        || dequeue.get("type").and_then(Value::as_str) != Some("queue-operation")
        || dequeue.get("operation").and_then(Value::as_str) != Some("dequeue")
        || dequeue.get("sessionId").and_then(Value::as_str) != Some(session_id)
    {
        return Err(ReplayError::continuation(
            "Claude native queue resume envelope is malformed",
        ));
    }
    if continue_event.get("type").and_then(Value::as_str) != Some("user")
        || continue_event.get("isMeta").and_then(Value::as_bool) != Some(true)
        || exact_claude_event_text(continue_event) != Some("Continue from where you left off.")
        || no_response_event.get("type").and_then(Value::as_str) != Some("assistant")
        || exact_claude_event_text(no_response_event) != Some("No response requested.")
        || nonce_event.get("type").and_then(Value::as_str) != Some("user")
        || exact_claude_event_text(nonce_event) != Some(nonce)
    {
        return Err(ReplayError::continuation(
            "Claude native resume message envelope is malformed",
        ));
    }
    let continue_uuid = required_event_uuid(continue_event, "continue")?;
    let no_response_uuid = required_event_uuid(no_response_event, "no-response")?;
    let nonce_uuid = required_event_uuid(nonce_event, "nonce")?;
    if continue_event.get("parentUuid").and_then(Value::as_str) != Some(&boundary_uuid)
        || no_response_event.get("parentUuid").and_then(Value::as_str) != Some(&continue_uuid)
        || nonce_event.get("parentUuid").and_then(Value::as_str) != Some(&no_response_uuid)
    {
        return Err(ReplayError::continuation(
            "Claude resume envelope is not attached directly to the boundary observation",
        ));
    }

    let mut remove_indexes: BTreeSet<usize> = (boundary_index + 1..boundary_index + 6).collect();
    let mut removed_parent_by_uuid = BTreeMap::from([
        (continue_uuid.clone(), boundary_uuid.clone()),
        (no_response_uuid.clone(), continue_uuid),
        (nonce_uuid.clone(), no_response_uuid),
    ]);
    let last_prompt_indexes: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            (event.get("type").and_then(Value::as_str) == Some("last-prompt")
                && event.get("lastPrompt").and_then(Value::as_str) == Some(nonce)
                && event.get("sessionId").and_then(Value::as_str) == Some(session_id))
            .then_some(index)
        })
        .collect();
    if last_prompt_indexes.is_empty() {
        return Err(ReplayError::continuation(
            "resumed Claude session has no nonce last-prompt metadata",
        ));
    }
    remove_indexes.extend(last_prompt_indexes);

    let mut attachment_parent = nonce_uuid;
    let mut previous_attachment_order = -1_i8;
    let mut seen_attachment_types = BTreeSet::new();
    loop {
        let matching: Vec<_> = events
            .iter()
            .enumerate()
            .filter(|(index, event)| {
                !remove_indexes.contains(index)
                    && event.get("type").and_then(Value::as_str) == Some("attachment")
                    && event.get("parentUuid").and_then(Value::as_str)
                        == Some(attachment_parent.as_str())
            })
            .collect();
        if matching.is_empty() {
            break;
        }
        if matching.len() != 1 {
            return Err(ReplayError::continuation(
                "Claude resume attachment branch is ambiguous",
            ));
        }
        let (index, event) = matching[0];
        let attachment = event.get("attachment").unwrap_or(&Value::Null);
        let attachment_type = attachment
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let attachment_order = match attachment_type {
            "agent_listing_delta" => 0,
            "skill_listing" => 1,
            "task_reminder" => 2,
            _ => -1,
        };
        let event_uuid = required_event_uuid(event, "resume attachment")?;
        if !valid_claude_resume_attachment(attachment)
            || !seen_attachment_types.insert(attachment_type.to_owned())
            || attachment_order <= previous_attachment_order
        {
            return Err(ReplayError::continuation(
                "unexpected attachment in Claude resume envelope",
            ));
        }
        previous_attachment_order = attachment_order;
        remove_indexes.insert(index);
        removed_parent_by_uuid.insert(event_uuid.clone(), attachment_parent);
        attachment_parent = event_uuid;
    }

    let mut cleaned_events = Vec::with_capacity(events.len() - remove_indexes.len());
    let mut first_real_assistant: Option<Value> = None;
    for (index, event) in events.into_iter().enumerate() {
        if remove_indexes.contains(&index) {
            continue;
        }
        let mut updated = event;
        if let Some(parent) = updated
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let resolved = resolve_claude_parent(parent, &removed_parent_by_uuid)?;
            updated["parentUuid"] = Value::String(resolved);
        }
        if index > boundary_index
            && first_real_assistant.is_none()
            && updated.get("type").and_then(Value::as_str) == Some("assistant")
            && updated.get("isSidechain").and_then(Value::as_bool) != Some(true)
        {
            first_real_assistant = Some(updated.clone());
        }
        cleaned_events.push(updated);
    }
    let first_real_assistant = first_real_assistant
        .ok_or_else(|| ReplayError::continuation("Claude produced no real continuation turn"))?;
    if first_real_assistant
        .get("parentUuid")
        .and_then(Value::as_str)
        != Some(boundary_uuid.as_str())
    {
        return Err(ReplayError::continuation(
            "first real resumed Claude assistant is not a child of the boundary observation",
        ));
    }
    for forbidden in [
        nonce,
        "Continue from where you left off.",
        "No response requested.",
    ] {
        if cleaned_events
            .iter()
            .any(|event| value_contains(event, forbidden))
        {
            return Err(ReplayError::continuation(
                "Claude resume transport text remains after native-session cleanup",
            ));
        }
    }
    let cleaned_boundary_index = cleaned_events
        .iter()
        .position(|event| event.get("uuid").and_then(Value::as_str) == Some(&boundary_uuid))
        .ok_or_else(|| ReplayError::continuation("cleaned Claude session lost its boundary"))?;
    let continued_steps = cleaned_events
        .iter()
        .skip(cleaned_boundary_index + 1)
        .filter(|event| {
            main_event(event)
                && event.get("type").and_then(Value::as_str) == Some("assistant")
                && event
                    .get("message")
                    .and_then(|message| message.get("stop_reason"))
                    .is_some_and(|reason| !reason.is_null())
        })
        .count();
    if continued_steps == 0 {
        return Err(ReplayError::continuation(
            "cleaned Claude session has no complete continuation turn",
        ));
    }
    let mut output = cleaned_events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .replay_context(
            ReplayErrorKind::Internal,
            "serialize cleaned Claude session",
        )?
        .join("\n");
    output.push('\n');
    Ok((output, continued_steps))
}

fn exact_claude_event_text(event: &Value) -> Option<&str> {
    let content = event.get("message")?.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    let blocks = content.as_array()?;
    if blocks.len() == 1 && blocks[0].get("type").and_then(Value::as_str) == Some("text") {
        return blocks[0].get("text").and_then(Value::as_str);
    }
    None
}

fn required_event_uuid(event: &Value, context: &str) -> Result<String, ReplayError> {
    event
        .get("uuid")
        .and_then(Value::as_str)
        .filter(|uuid| !uuid.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ReplayError::continuation(format!("Claude {context} event lacks a UUID")))
}

fn valid_claude_resume_attachment(attachment: &Value) -> bool {
    match attachment.get("type").and_then(Value::as_str) {
        Some("task_reminder") => {
            attachment
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
                && attachment.get("itemCount").and_then(Value::as_u64) == Some(0)
        }
        Some("agent_listing_delta") => {
            let Some(added_lines) = attachment.get("addedLines").and_then(Value::as_array) else {
                return false;
            };
            let Some(added_types) = attachment.get("addedTypes").and_then(Value::as_array) else {
                return false;
            };
            attachment.get("isInitial").and_then(Value::as_bool) == Some(true)
                && attachment
                    .get("showConcurrencyNote")
                    .and_then(Value::as_bool)
                    .is_some()
                && added_lines.iter().all(Value::is_string)
                && added_types.iter().all(Value::is_string)
                && added_lines.len() == added_types.len()
                && attachment
                    .get("removedTypes")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
        }
        Some("skill_listing") => {
            let Some(names) = attachment.get("names").and_then(Value::as_array) else {
                return false;
            };
            attachment.get("isInitial").and_then(Value::as_bool) == Some(true)
                && attachment.get("content").and_then(Value::as_str).is_some()
                && names.iter().all(Value::is_string)
                && attachment.get("skillCount").and_then(Value::as_u64) == Some(names.len() as u64)
        }
        _ => false,
    }
}

fn resolve_claude_parent(
    mut parent: String,
    removed_parent_by_uuid: &BTreeMap<String, String>,
) -> Result<String, ReplayError> {
    let mut seen = BTreeSet::new();
    while let Some(next) = removed_parent_by_uuid.get(&parent) {
        if !seen.insert(parent.clone()) {
            return Err(ReplayError::continuation(
                "cycle in Claude resume transport parent chain",
            ));
        }
        parent = next.clone();
    }
    Ok(parent)
}

fn value_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(value) => value.contains(needle),
        Value::Array(values) => values.iter().any(|value| value_contains(value, needle)),
        Value::Object(values) => values.values().any(|value| value_contains(value, needle)),
        _ => false,
    }
}
fn expected_claude_max_turn_exit(stdout: &[u8], max_turns: Option<usize>) -> bool {
    let Some(max_turns) = max_turns else {
        return false;
    };
    for line in String::from_utf8_lossy(stdout).lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        return event.get("type").and_then(Value::as_str) == Some("result")
            && event.get("subtype").and_then(Value::as_str) == Some("error_max_turns")
            && event.get("terminal_reason").and_then(Value::as_str) == Some("max_turns")
            && event.get("num_turns").and_then(Value::as_u64) == Some((max_turns + 1) as u64);
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use serde_json::{json, Value};

    use super::{
        claude_boundary_tool_use_ids, claude_canonical_messages, execute_claude_tool_with_policy,
        expected_claude_max_turn_exit, rebuild_claude, run_bash, validate_claude_tool_policy,
        wildcard_match,
    };
    use crate::adapter::{build_plan, run, RunContext};
    use crate::claude_resume::ResumeTransportManifest;
    use crate::journal::Journal;
    use crate::model::{
        AdapterPlan, AgentKind, FreshObservation, PlaybackRequest, ReplayMode, ToolCall,
    };

    fn claude_tool_call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            ordinal: 1,
            call_id: "call-1".into(),
            name: name.into(),
            arguments,
            original_observation: Value::Null,
            original_is_error: false,
            native: Value::Null,
        }
    }

    #[test]
    fn claude_max_turn_exit_requires_exact_terminal_result() {
        let event = json!({
            "type": "result",
            "subtype": "error_max_turns",
            "terminal_reason": "max_turns",
            "num_turns": 2,
        })
        .to_string();
        assert!(expected_claude_max_turn_exit(event.as_bytes(), Some(1)));
        assert!(!expected_claude_max_turn_exit(event.as_bytes(), None));
        assert!(!expected_claude_max_turn_exit(event.as_bytes(), Some(2)));

        let wrong_terminal = json!({
            "type": "result",
            "subtype": "error_max_turns",
            "terminal_reason": "other",
            "num_turns": 2,
        })
        .to_string();
        assert!(!expected_claude_max_turn_exit(
            wrong_terminal.as_bytes(),
            Some(1)
        ));

        let trailing_json = format!("{event}\n{}", json!({"type": "assistant"}));
        assert!(!expected_claude_max_turn_exit(
            trailing_json.as_bytes(),
            Some(1)
        ));
        let trailing_noise = format!("{event}\nnot-json");
        assert!(expected_claude_max_turn_exit(
            trailing_noise.as_bytes(),
            Some(1)
        ));
    }

    #[test]
    fn claude_input_validation_errors_are_replayed_without_execution() {
        let workspace = tempfile::tempdir().unwrap();
        let mut call = claude_tool_call("Read", json!({}));
        call.original_observation = json!("<tool_use_error>file_path is missing</tool_use_error>");
        call.original_is_error = true;

        let observation =
            execute_claude_tool_with_policy(&call, workspace.path(), true, None).unwrap();
        assert!(observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("stale_source_observation"))
        );
    }

    #[test]
    fn claude_read_only_explore_agent_is_marked_opaque() {
        let workspace = tempfile::tempdir().unwrap();
        let mut call = claude_tool_call(
            "Agent",
            json!({
                "description": "Inspect code",
                "prompt": "Find the relevant files",
                "subagent_type": "Explore",
            }),
        );
        call.original_observation = json!([{
            "type": "text",
            "text": "Async agent launched successfully.",
        }]);

        let observation =
            execute_claude_tool_with_policy(&call, workspace.path(), true, None).unwrap();
        assert!(!observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("stale_source_observation"))
        );
    }

    #[test]
    fn claude_read_only_task_output_is_marked_opaque() {
        let workspace = tempfile::tempdir().unwrap();
        let mut call = claude_tool_call(
            "TaskOutput",
            json!({
                "task_id": "a7e5a5bf351a66db5",
                "block": true,
                "timeout": 120000,
            }),
        );
        call.original_observation = json!([{
            "type": "text",
            "text": "Explore agent result",
        }]);

        let observation =
            execute_claude_tool_with_policy(&call, workspace.path(), true, None).unwrap();
        assert!(!observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("stale_source_observation"))
        );
    }

    #[test]
    fn stale_observations_fail_closed_by_default() {
        for call in [
            claude_tool_call(
                "Agent",
                json!({
                    "description": "Inspect code",
                    "prompt": "Find files",
                    "subagent_type": "Explore",
                }),
            ),
            claude_tool_call("TaskOutput", json!({"task_id": "task-1"})),
            claude_tool_call("TaskCreate", json!({"subject": "work"})),
            claude_tool_call("TodoWrite", json!({"todos": []})),
        ] {
            let error = validate_claude_tool_policy(&call, false).unwrap_err();
            assert!(error.to_string().contains("--allow-stale-observations"));
        }

        let find = claude_tool_call("Find", json!({"pattern": "*.rs"}));
        assert!(validate_claude_tool_policy(&find, true).is_err());
    }

    #[test]
    fn stale_observations_are_explicitly_degraded() {
        let workspace = tempfile::tempdir().unwrap();
        let mut call = claude_tool_call("TaskOutput", json!({"task_id": "task-1"}));
        call.original_observation = json!("source observation");

        validate_claude_tool_policy(&call, true).unwrap();
        let observation =
            execute_claude_tool_with_policy(&call, workspace.path(), true, None).unwrap();

        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("degradation_reason"),
            Some(&json!("stale_source_observation"))
        );
        assert_eq!(
            observation.metadata.get("source_call_id"),
            Some(&json!(call.call_id))
        );
    }

    #[test]
    fn prepare_only_executes_no_historical_tool() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        let state = temporary.path().join("state");
        let output = temporary.path().join("output");
        let trajectory = temporary.path().join("trajectory.jsonl");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(output.join("native")).unwrap();
        let marker = workspace.join("must-not-exist");
        let events = [
            json!({
                "type": "assistant", "uuid": "assistant-1", "parentUuid": null,
                "sessionId": "session", "version": "2.1.220",
                "message": {"id": "message-1", "stop_reason": "tool_use", "content": [{
                    "type": "tool_use", "id": "tool-1", "name": "Bash",
                    "input": {"command": format!("touch {}", marker.display())}
                }]}
            }),
            json!({
                "type": "user", "uuid": "result-1", "parentUuid": "assistant-1",
                "sourceToolAssistantUUID": "assistant-1", "sessionId": "session",
                "version": "2.1.220", "message": {"content": [{
                    "type": "tool_result", "tool_use_id": "tool-1", "content": "old"
                }]}
            }),
            json!({
                "type": "assistant", "uuid": "assistant-2", "parentUuid": "result-1",
                "sessionId": "session", "version": "2.1.220",
                "message": {"id": "message-2", "stop_reason": "end_turn", "content": [{
                    "type": "text", "text": "next"
                }]}
            }),
        ];
        fs::write(
            &trajectory,
            events
                .iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let request = PlaybackRequest {
            agent: AgentKind::ClaudeCode,
            trajectory,
            after_step: 1,
            workspace,
            state_dir: state.clone(),
            output_dir: output.clone(),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: Some("test".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        };
        let plan = build_plan(&request).unwrap();
        let mut journal = Journal::open(&state).unwrap();
        let context = RunContext {
            request: &request,
            state_dir: &state,
            output_dir: &output,
            launch: None,
            session_id: "session",
            nonce: "nonce",
        };

        let outcome = run(&plan, &context, &mut journal).unwrap();

        assert_eq!(outcome.status, "prepared");
        assert!(outcome.observations.is_empty());
        assert!(!marker.exists());
    }

    #[test]
    fn claude_read_directory_and_missing_nested_path_are_tool_errors() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("directory")).unwrap();

        for file_path in ["directory", "missing/nested/file.txt"] {
            let observation = execute_claude_tool_with_policy(
                &claude_tool_call("Read", json!({"file_path": file_path})),
                workspace.path(),
                false,
                None,
            )
            .unwrap();
            assert!(observation.is_error);
            assert_eq!(observation.return_code, Some(1));
            assert!(observation
                .content
                .as_str()
                .unwrap()
                .contains("Read failed"));
        }
    }

    #[test]
    fn claude_grep_accepts_search_and_files_aliases() {
        let workspace = tempfile::tempdir().unwrap();
        let source = workspace.path().join("source.txt");
        fs::write(&source, "first\nneedle here\nlast\n").unwrap();
        let observation = execute_claude_tool_with_policy(
            &claude_tool_call("Grep", json!({"search": "needle", "files": source})),
            workspace.path(),
            false,
            None,
        )
        .unwrap();
        assert!(!observation.is_error);
        let content = observation.content.as_str().unwrap();
        assert!(content.contains("needle here"));
        assert!(!content.contains(":1:first"));
        assert!(!content.contains(":3:last"));
    }

    #[test]
    fn claude_grep_rejects_an_empty_pattern() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("source.txt"), "content").unwrap();
        let observation = execute_claude_tool_with_policy(
            &claude_tool_call("Grep", json!({"pattern": ""})),
            workspace.path(),
            false,
            None,
        )
        .unwrap();
        assert!(observation.is_error);
        assert_eq!(observation.return_code, Some(1));
        assert!(observation
            .content
            .as_str()
            .unwrap()
            .contains("missing or empty"));
    }

    #[test]
    fn claude_fixture_builds_one_complete_batch() {
        let request = PlaybackRequest {
            agent: AgentKind::ClaudeCode,
            trajectory: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/claude_bash_one_step.jsonl"),
            after_step: 1,
            workspace: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/state"),
            output_dir: PathBuf::from("/tmp/output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: Some("test".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        };
        if !request.trajectory.exists() {
            return;
        }
        let AdapterPlan::ClaudeCode(plan) = build_plan(&request).unwrap() else {
            panic!("Claude fixture produced a non-Claude plan");
        };
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0].tool_calls[0].name, "Bash");
        let replacements = BTreeMap::from([(
            "tool-1".to_owned(),
            FreshObservation {
                call_id: "tool-1".into(),
                content: json!("fresh observation"),
                is_error: false,
                return_code: Some(0),
                duration_ms: 1,
                truncated: false,
                metadata: BTreeMap::new(),
            },
        )]);
        let rebuilt = rebuild_claude(&plan, &replacements).unwrap();
        let canonical_messages = claude_canonical_messages(&rebuilt).unwrap();
        assert_eq!(canonical_messages.len(), 3);
        assert_eq!(
            canonical_messages[2]["content"][0]["content"],
            "fresh observation"
        );
        let manifest = ResumeTransportManifest::create(
            "session-1",
            vec!["tool-1".into()],
            canonical_messages,
            "__PVISOR_NATIVE_REPLAY_0123456789abcdef__".into(),
        )
        .unwrap();
        assert_eq!(manifest.canonical_message_count, 3);
        assert_eq!(manifest.boundary_observation_sha256.len(), 1);
    }

    #[test]
    fn claude_groups_interleaved_stream_fragments_into_one_logical_batch() {
        let temp = tempfile::tempdir().unwrap();
        let trajectory = temp.path().join("trajectory.jsonl");
        let events = [
            json!({
                "type":"assistant", "uuid":"assistant-1", "parentUuid":null,
                "sessionId":"session", "version":"2.1.220",
                "message":{"id":"message-1","stop_reason":null,"content":[
                    {"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"true"}}
                ]}
            }),
            json!({
                "type":"user", "uuid":"result-1", "parentUuid":"assistant-1",
                "sourceToolAssistantUUID":"assistant-1",
                "sessionId":"session", "version":"2.1.220",
                "message":{"content":[
                    {"type":"tool_result","tool_use_id":"tool-1","content":"old-1"}
                ]}
            }),
            json!({
                "type":"assistant", "uuid":"assistant-2", "parentUuid":"result-1",
                "sessionId":"session", "version":"2.1.220",
                "message":{"id":"message-1","stop_reason":"tool_use","content":[
                    {"type":"tool_use","id":"tool-2","name":"Bash","input":{"command":"true"}}
                ]}
            }),
            json!({
                "type":"user", "uuid":"result-2", "parentUuid":"assistant-2",
                "sourceToolAssistantUUID":"assistant-2",
                "sessionId":"session", "version":"2.1.220",
                "message":{"content":[
                    {"type":"tool_result","tool_use_id":"tool-2","content":"old-2"}
                ]}
            }),
            json!({
                "type":"assistant", "uuid":"assistant-next", "parentUuid":"result-2",
                "sessionId":"session", "version":"2.1.220",
                "message":{"id":"message-2","stop_reason":"end_turn","content":[
                    {"type":"text","text":"next"}
                ]}
            }),
        ];
        fs::write(
            &trajectory,
            events
                .iter()
                .map(|event| serde_json::to_string(event).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let request = PlaybackRequest {
            agent: AgentKind::ClaudeCode,
            trajectory,
            after_step: 1,
            workspace: PathBuf::from("/tmp"),
            state_dir: PathBuf::from("/tmp/state"),
            output_dir: PathBuf::from("/tmp/output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: Some("test".into()),
            disable_thinking: false,
            boundary_user_prompt: None,
        };
        let AdapterPlan::ClaudeCode(plan) = build_plan(&request).unwrap() else {
            panic!("Claude fixture produced a non-Claude plan");
        };
        assert_eq!(plan.batches.len(), 1);
        assert_eq!(plan.batches[0].tool_calls.len(), 2);
        assert_eq!(
            claude_boundary_tool_use_ids(&plan),
            vec!["tool-1".to_owned(), "tool-2".to_owned()]
        );
        assert_eq!(plan.native["boundary_result_index"], 3);
        assert_eq!(plan.original_next_action.as_ref().unwrap()["text"], "next");
        assert_eq!(plan.original_next_action.as_ref().unwrap()["reasoning"], "");

        let replacements = plan
            .calls()
            .map(|call| {
                (
                    call.call_id.clone(),
                    FreshObservation {
                        call_id: call.call_id.clone(),
                        content: Value::String(format!("fresh-{}", call.call_id)),
                        is_error: false,
                        return_code: Some(0),
                        duration_ms: 1,
                        truncated: false,
                        metadata: BTreeMap::new(),
                    },
                )
            })
            .collect();
        let rebuilt = rebuild_claude(&plan, &replacements).unwrap();
        let rebuilt: Vec<Value> = rebuilt
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rebuilt.last().unwrap()["uuid"], "result-2");
        assert_eq!(rebuilt.len(), 4);
    }

    #[test]
    fn bash_timeout_kills_the_historical_process_group() {
        let workspace = tempfile::tempdir().unwrap();
        let log = workspace.path().join("bash.log");
        let started = Instant::now();
        let (content, is_error, return_code, truncated) =
            run_bash("sleep 5", workspace.path(), Duration::from_millis(50), &log).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(is_error);
        assert_eq!(return_code, Some(124));
        assert!(content.contains("timed out"));
        assert!(!truncated);
    }

    #[test]
    fn bash_reports_truncation_and_background_cleanup() {
        let workspace = tempfile::tempdir().unwrap();
        let large_log = workspace.path().join("large.log");
        let (_, is_error, _, truncated) = run_bash(
            "yes x | head -c 6291456",
            workspace.path(),
            Duration::from_secs(2),
            &large_log,
        )
        .unwrap();
        assert!(!is_error);
        assert!(truncated);

        let background_log = workspace.path().join("background.log");
        let (content, is_error, _, _) = run_bash(
            "sleep 30 &",
            workspace.path(),
            Duration::from_secs(2),
            &background_log,
        )
        .unwrap();
        assert!(is_error);
        assert!(content.contains("background descendants were terminated"));
    }

    #[test]
    fn wildcard_supports_recursive_style_patterns() {
        assert!(wildcard_match("**/*.rs", "src/lib.rs"));
        assert!(!wildcard_match("*.toml", "src/lib.rs"));
    }
}
