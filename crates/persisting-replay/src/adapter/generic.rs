//! Replay support for agents whose native transcript is a JSONL event stream.
//!
//! OpenCode prints `run --format=json` events, while Codex persists
//! `response_item` events in its rollout JSONL.  Both formats carry the
//! assistant tool call and its observation in the transcript, so the replay
//! prefix can be rebuilt without an Agent SDK.  The live phase is delegated to
//! the native CLI after the reconstructed transcript has been staged.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{
    MAX_TOOL_OUTPUT_BYTES, RunContext, agent_command, check_boundary, prepared_outcome,
    with_boundary_user_prompt_metadata,
};
use crate::codex_bridge::{CodexBridgeHandle, PromptMode};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write, atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AgentKind, FreshObservation, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan, ToolBatch,
    ToolCall,
};
use crate::process::{ProcessSpec, run_process};

#[derive(Debug, Clone, Copy)]
pub(super) enum NativeJsonlAgent {
    Opencode,
    Codex,
}

impl NativeJsonlAgent {
    fn kind(self) -> AgentKind {
        match self {
            Self::Opencode => AgentKind::Opencode,
            Self::Codex => AgentKind::Codex,
        }
    }

    fn label(self) -> &'static str {
        self.kind().as_str()
    }
}

#[derive(Debug, Clone)]
struct CallRecord {
    call_event: usize,
    output_event: usize,
    call_id: String,
    name: String,
    arguments: Value,
    observation: Value,
    is_error: bool,
    complete: bool,
}

#[derive(Debug, Clone)]
struct TurnRecord {
    start_event: usize,
    end_event: usize,
    text: String,
    reasoning: String,
    calls: Vec<CallRecord>,
}

type ParsedNative = (Vec<TurnRecord>, Option<String>, Option<String>);

pub(super) fn build(
    request: &PlaybackRequest,
    agent: NativeJsonlAgent,
) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let events = parse_jsonl(&raw, agent.label())?;
    let (turns, user_prompt, session_id) = match agent {
        NativeJsonlAgent::Opencode => parse_opencode(&events)?,
        NativeJsonlAgent::Codex => parse_codex(&events)?,
    };
    let complete_turns = turns
        .iter()
        .filter(|turn| !turn.calls.is_empty() && turn.calls.iter().all(|call| call.complete))
        .collect::<Vec<_>>();
    check_boundary(request.after_step, complete_turns.len())?;
    let selected = &complete_turns[..request.after_step];
    let boundary_end = selected
        .last()
        .map(|turn| turn.end_event)
        .ok_or_else(|| ReplayError::trajectory("native JSONL replay boundary has no turn"))?;
    let prefix_model_turns = turns
        .iter()
        .filter(|turn| turn.end_event <= boundary_end)
        .count();
    let original_next_action = turns
        .iter()
        .filter(|turn| turn.start_event > boundary_end)
        .find(|turn| is_actionable_turn(turn))
        .map(turn_signature);
    let batches = selected
        .iter()
        .enumerate()
        .map(|(index, turn)| ToolBatch {
            ordinal: index + 1,
            native_locator: format!("events:{}-{}", turn.start_event, turn.end_event),
            tool_calls: turn
                .calls
                .iter()
                .enumerate()
                .map(|(ordinal, call)| ToolCall {
                    ordinal: ordinal + 1,
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    original_observation: call.observation.clone(),
                    original_is_error: call.is_error,
                    native: json!({
                        "call_event": call.call_event,
                        "output_event": call.output_event,
                    }),
                })
                .collect(),
            assistant_text: turn.text.clone(),
            native: json!({
                "start_event": turn.start_event,
                "end_event": turn.end_event,
            }),
        })
        .collect();
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
        native: json!({
            "format": agent.label(),
            "events": events,
            "user_prompt": user_prompt,
            "session_id": session_id,
        }),
        original_next_action,
    })
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    agent: NativeJsonlAgent,
) -> Result<ReplayOutcome, ReplayError> {
    let events = plan
        .native
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| ReplayError::trajectory("native JSONL plan has no events"))?;
    let boundary_end = plan
        .batches
        .last()
        .and_then(|batch| batch.native.get("end_event"))
        .and_then(Value::as_u64)
        .ok_or_else(|| ReplayError::trajectory("native JSONL batch has no end event"))?
        as usize;
    if boundary_end >= events.len() {
        return Err(ReplayError::trajectory(
            "native JSONL boundary is out of bounds",
        ));
    }
    let mut reconstructed_events = events[..=boundary_end].to_vec();
    let mut observations = Vec::new();
    for call in plan.calls() {
        let fresh = execute_call(call, context)?;
        let output_event = call
            .native
            .get("output_event")
            .and_then(Value::as_u64)
            .ok_or_else(|| ReplayError::trajectory("native JSONL call has no output event"))?
            as usize;
        match agent {
            NativeJsonlAgent::Opencode => replace_opencode_observation(
                reconstructed_events.get_mut(output_event).ok_or_else(|| {
                    ReplayError::trajectory("OpenCode call event is out of bounds")
                })?,
                &fresh,
            )?,
            NativeJsonlAgent::Codex => replace_codex_observation(
                reconstructed_events.get_mut(output_event).ok_or_else(|| {
                    ReplayError::trajectory("Codex output event is out of bounds")
                })?,
                &fresh,
            )?,
        }
        observations.push(fresh);
    }
    let prepared = context.output_dir.join("native/prepared-prefix.jsonl");
    write_jsonl(&prepared, &reconstructed_events)?;
    journal.append(
        "session_rebuilt",
        [(
            "prepared_only".into(),
            json!(context.request.mode == ReplayMode::PrepareOnly),
        )],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(prepared, context.request));
    }

    let replayed = context
        .output_dir
        .join("native/reconstructed-trajectory.jsonl");
    write_jsonl(&replayed, &reconstructed_events)?;
    if context.request.mode == ReplayMode::ReplayOnly {
        write_comparison(context, plan, &observations)?;
        return Ok(ReplayOutcome {
            status: "replayed".into(),
            reconstructed_path: Some(replayed),
            continued_path: None,
            observations,
            continued_steps: 0,
            metadata: with_boundary_user_prompt_metadata(
                json!({"native_cli": agent.label()}),
                context.request,
                false,
            ),
        });
    }

    let (continued, continued_steps) = continue_native_cli(
        plan,
        context,
        journal,
        agent,
        &reconstructed_events,
        &replayed,
    )?;
    write_comparison(context, plan, &observations)?;
    if continued_steps == 0 {
        return Err(ReplayError::continuation(format!(
            "{} produced no continuation turns; see logs",
            agent.label()
        )));
    }
    let mut metadata = json!({"native_cli": agent.label()});
    if matches!(agent, NativeJsonlAgent::Codex) {
        let prompt_mode = if context.request.boundary_user_prompt().is_some() {
            PromptMode::ExplicitUserPrompt
        } else {
            PromptMode::TransportNonce
        };
        metadata["codex_resume_transport"] = json!({
            "prompt_mode": prompt_mode.as_str(),
            "removed_before_model_request": prompt_mode == PromptMode::TransportNonce,
            "removed_from_native_trajectory": prompt_mode == PromptMode::TransportNonce,
        });
    }
    Ok(ReplayOutcome {
        status: "completed".into(),
        reconstructed_path: None,
        continued_path: Some(continued),
        observations,
        continued_steps,
        metadata: with_boundary_user_prompt_metadata(
            metadata,
            context.request,
            context.request.boundary_user_prompt().is_some(),
        ),
    })
}

fn parse_jsonl(raw: &[u8], label: &str) -> Result<Vec<Value>, ReplayError> {
    let source = std::str::from_utf8(raw).map_err(|error| {
        ReplayError::trajectory(format!("{label} trajectory is not UTF-8: {error}"))
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
                    "{label} JSONL line {} must be an object",
                    index + 1
                )));
            }
            Err(_) if index + 1 == physical.len() && !source.ends_with('\n') => break,
            Err(error) => {
                return Err(ReplayError::trajectory(format!(
                    "invalid {label} JSONL line {}: {error}",
                    index + 1
                )));
            }
        }
    }
    if events.is_empty() {
        return Err(ReplayError::trajectory(format!(
            "{label} trajectory has no JSON events"
        )));
    }
    Ok(events)
}

fn parse_opencode(events: &[Value]) -> Result<ParsedNative, ReplayError> {
    let mut user_prompt = None;
    let mut session_id = None;
    let mut turns = Vec::new();
    let mut current: Option<TurnRecord> = None;
    for (index, event) in events.iter().enumerate() {
        session_id = session_id.or_else(|| {
            event
                .get("sessionID")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        match event.get("type").and_then(Value::as_str) {
            Some("user") => {
                if user_prompt.is_none() {
                    user_prompt = opencode_text(event);
                }
            }
            Some("step_start") => {
                if let Some(previous) = current.take() {
                    turns.push(previous);
                }
                current = Some(TurnRecord {
                    start_event: index,
                    end_event: index,
                    text: String::new(),
                    reasoning: String::new(),
                    calls: Vec::new(),
                });
            }
            Some("text") | Some("reasoning") | Some("tool_use") => {
                let Some(turn) = current.as_mut() else {
                    continue;
                };
                turn.end_event = index;
                let part = event.get("part").cloned().unwrap_or_else(|| json!({}));
                let part_type = part
                    .get("type")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("type").and_then(Value::as_str));
                match part_type {
                    Some("text") => {
                        append_text(&mut turn.text, part.get("text").and_then(Value::as_str))
                    }
                    Some("reasoning") => append_text(
                        &mut turn.reasoning,
                        part.get("text").and_then(Value::as_str),
                    ),
                    Some("tool") | Some("tool_use") => {
                        let id = part
                            .get("callID")
                            .or_else(|| part.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("opencode-{index}"));
                        let name = part
                            .get("tool")
                            .or_else(|| part.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let state = part.get("state").cloned().unwrap_or_else(|| json!({}));
                        let arguments = state.get("input").cloned().unwrap_or_else(|| json!({}));
                        let is_error = state.get("status").and_then(Value::as_str) == Some("error")
                            || state.get("error").is_some_and(|value| !value.is_null());
                        let observation = state.get("output").cloned().unwrap_or(Value::Null);
                        let complete = !observation.is_null()
                            || is_error
                            || state.get("status").and_then(Value::as_str) == Some("completed");
                        // OpenCode can emit more than one `tool_use` event for
                        // a call while its state transitions from pending to
                        // completed. Keep one call record and retain the final
                        // observation/event location.
                        if let Some(call) = turn.calls.iter_mut().find(|call| call.call_id == id) {
                            call.output_event = index;
                            if !name.is_empty() {
                                call.name = name;
                            }
                            if arguments != json!({}) {
                                call.arguments = arguments;
                            }
                            if !observation.is_null() {
                                call.observation = observation;
                            }
                            call.is_error |= is_error;
                            call.complete |= complete;
                        } else {
                            turn.calls.push(CallRecord {
                                call_event: index,
                                output_event: index,
                                call_id: id,
                                name,
                                arguments,
                                observation,
                                is_error,
                                complete,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("step_finish") => {
                if let Some(mut turn) = current.take() {
                    turn.end_event = index;
                    turns.push(turn);
                }
            }
            _ => {}
        }
    }
    if let Some(turn) = current {
        turns.push(turn);
    }
    if user_prompt.is_none() {
        return Err(ReplayError::trajectory(
            "OpenCode trajectory has no user prompt",
        ));
    }
    Ok((turns, user_prompt, session_id))
}

fn parse_codex(events: &[Value]) -> Result<ParsedNative, ReplayError> {
    let mut user_prompt = None;
    let mut session_id = None;
    let mut turns = Vec::new();
    let mut current: Option<TurnRecord> = None;
    let mut pending_outputs: Vec<(String, usize, Value, bool)> = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(payload) = event.get("payload") else {
            continue;
        };
        if event_type == "session_meta" {
            session_id = session_id.or_else(|| {
                payload
                    .get("id")
                    .or_else(|| payload.get("session_id"))
                    .or_else(|| event.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
            });
            continue;
        }
        if event_type != "response_item" {
            continue;
        }
        match payload.get("type").and_then(Value::as_str) {
            Some("message") => {
                let role = payload
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if role == "user" {
                    if user_prompt.is_none() {
                        user_prompt = codex_message_text(payload, "input_text");
                    }
                    if let Some(turn) = current.take() {
                        turns.push(turn);
                    }
                } else if role == "assistant" {
                    if let Some(turn) = current.take()
                        && (!turn.calls.is_empty()
                            || !turn.text.is_empty()
                            || !turn.reasoning.is_empty())
                    {
                        turns.push(turn);
                    }
                    current = Some(TurnRecord {
                        start_event: index,
                        end_event: index,
                        text: codex_message_text(payload, "output_text").unwrap_or_default(),
                        reasoning: String::new(),
                        calls: Vec::new(),
                    });
                }
            }
            Some("reasoning") => {
                let turn = current.get_or_insert_with(|| TurnRecord {
                    start_event: index,
                    end_event: index,
                    text: String::new(),
                    reasoning: String::new(),
                    calls: Vec::new(),
                });
                turn.end_event = index;
                if let Some(summary) = payload.get("summary").and_then(Value::as_array) {
                    for item in summary {
                        append_text(
                            &mut turn.reasoning,
                            item.get("text").and_then(Value::as_str),
                        );
                    }
                }
            }
            Some("function_call") | Some("custom_tool_call") => {
                let turn = current.get_or_insert_with(|| TurnRecord {
                    start_event: index,
                    end_event: index,
                    text: String::new(),
                    reasoning: String::new(),
                    calls: Vec::new(),
                });
                turn.end_event = index;
                let call_id = payload
                    .get("call_id")
                    .or_else(|| payload.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        ReplayError::trajectory(format!(
                            "Codex function_call at event {index} has no call_id"
                        ))
                    })?;
                let arguments = match payload.get("arguments").or_else(|| payload.get("input")) {
                    Some(Value::String(raw)) => {
                        serde_json::from_str(raw).unwrap_or_else(|_| json!(raw))
                    }
                    Some(value) => value.clone(),
                    None => json!({}),
                };
                let name = payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                turn.calls.push(CallRecord {
                    call_event: index,
                    output_event: usize::MAX,
                    call_id,
                    name,
                    arguments,
                    observation: Value::Null,
                    is_error: false,
                    complete: false,
                });
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                let call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if let Some(turn) = current.as_mut() {
                    turn.end_event = index;
                    if let Some(call) = turn.calls.iter_mut().find(|call| call.call_id == call_id) {
                        call.output_event = index;
                        call.observation = payload.get("output").cloned().unwrap_or(Value::Null);
                        call.complete = true;
                        call.is_error = payload
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|status| status != "completed")
                            || payload.get("error").is_some_and(|value| !value.is_null());
                    } else {
                        pending_outputs.push((
                            call_id.to_owned(),
                            index,
                            payload.get("output").cloned().unwrap_or(Value::Null),
                            false,
                        ));
                    }
                }
            }
            _ => {}
        }
        if !pending_outputs.is_empty() {
            for (call_id, output_event, observation, is_error) in pending_outputs.drain(..) {
                if let Some(turn) = current.as_mut()
                    && let Some(call) = turn.calls.iter_mut().find(|call| call.call_id == call_id)
                {
                    call.output_event = output_event;
                    call.observation = observation;
                    call.is_error = is_error;
                    call.complete = true;
                }
            }
        }
    }
    if let Some(turn) = current {
        turns.push(turn);
    }
    if user_prompt.is_none() {
        return Err(ReplayError::trajectory(
            "Codex trajectory has no user message",
        ));
    }
    // A response_item session_meta carries the ID in its payload, but older
    // rollouts put it directly in the event. Accept both forms.
    if session_id.is_none() {
        session_id = events.iter().find_map(|event| {
            (event.get("type").and_then(Value::as_str) == Some("session_meta"))
                .then(|| {
                    event
                        .get("payload")
                        .and_then(|payload| payload.get("id"))
                        .or_else(|| event.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .flatten()
        });
    }
    Ok((turns, user_prompt, session_id))
}

fn turn_signature(turn: &TurnRecord) -> Value {
    json!({
        "text": turn.text,
        "reasoning": turn.reasoning,
        "tools": turn.calls.iter().map(|call| json!({"name": call.name, "arguments": call.arguments})).collect::<Vec<_>>(),
    })
}

fn is_actionable_turn(turn: &TurnRecord) -> bool {
    !turn.text.trim().is_empty() || !turn.calls.is_empty()
}

fn opencode_text(event: &Value) -> Option<String> {
    event
        .get("parts")
        .and_then(Value::as_array)
        .or_else(|| event.get("part").and_then(Value::as_array))
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty())
}

fn codex_message_text(payload: &Value, wanted_type: &str) -> Option<String> {
    payload
        .get("content")
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|part| {
                    (part.get("type").and_then(Value::as_str) == Some(wanted_type))
                        .then(|| part.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|text| !text.is_empty())
}

fn append_text(target: &mut String, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return;
    };
    if !target.is_empty() {
        target.push('\n');
    }
    target.push_str(value);
}

fn execute_call(
    call: &ToolCall,
    context: &RunContext<'_>,
) -> Result<FreshObservation, ReplayError> {
    let started = Instant::now();
    let mut is_error = false;
    let mut return_code = None;
    let content = match execute_tool_value(&call.name, &call.arguments, context, call.ordinal) {
        Ok((content, code)) => {
            return_code = code;
            content
        }
        Err(error) => {
            is_error = true;
            Value::String(error.to_string())
        }
    };
    if return_code.is_some_and(|code| code != 0) {
        is_error = true;
    }
    Ok(FreshObservation {
        call_id: call.call_id.clone(),
        content,
        is_error,
        return_code,
        duration_ms: started.elapsed().as_millis(),
        truncated: false,
        metadata: Default::default(),
    })
}

fn execute_tool_value(
    name: &str,
    arguments: &Value,
    context: &RunContext<'_>,
    ordinal: usize,
) -> Result<(Value, Option<i32>), ReplayError> {
    let mut arguments = arguments.clone();
    if let Value::String(raw) = &arguments {
        arguments = match serde_json::from_str(raw) {
            Ok(parsed) => parsed,
            Err(_) if normalized_name(name) == "apply_patch" => {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedVersion,
                    "native apply_patch calls must provide structured JSON arguments",
                ));
            }
            Err(_) => json!({"command": raw}),
        };
    }
    let normalized = normalized_name(name);
    if matches!(
        normalized.as_str(),
        "bash" | "shell" | "exec" | "execute" | "terminal"
    ) || arguments.get("command").is_some()
        || arguments.get("cmd").is_some()
    {
        let command = arguments
            .get("command")
            .or_else(|| arguments.get("cmd"))
            .or_else(|| arguments.get("script"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ReplayError::trajectory(format!(
                    "{} tool {name:?} has no command",
                    context.request.agent.as_str()
                ))
            })?;
        let mut process = Command::new("/bin/sh");
        process
            .args(["-c", command])
            .current_dir(&context.request.workspace);
        let output = run_process(ProcessSpec {
            command: process,
            stdin: None,
            timeout: Duration::from_secs(30 * 60),
            termination_grace: Duration::from_secs(2),
            pipe_grace: Duration::from_millis(250),
            retained_bytes: MAX_TOOL_OUTPUT_BYTES,
            log_path: context.state_dir.join(format!("native-tool-{ordinal}.log")),
        })
        .map_err(|error| ReplayError::new(ReplayErrorKind::Executor, error.message))?;
        let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
        if !output.stderr_tail.is_empty() {
            if !rendered.is_empty() {
                rendered.push('\n');
            }
            rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
        }
        return Ok((Value::String(rendered), output.status.code()));
    }
    match normalized.as_str() {
        "read" | "cat" => {
            let path = tool_path(
                arguments
                    .get("path")
                    .or_else(|| arguments.get("filePath"))
                    .or_else(|| arguments.get("file_path")),
                context,
            )?;
            Ok((
                Value::String(String::from_utf8_lossy(&read_regular_file(&path)?).into_owned()),
                Some(0),
            ))
        }
        "write" => {
            let path = tool_path(
                arguments
                    .get("path")
                    .or_else(|| arguments.get("filePath"))
                    .or_else(|| arguments.get("file_path")),
                context,
            )?;
            let content = arguments
                .get("content")
                .or_else(|| arguments.get("file_text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .replay_context(ReplayErrorKind::Executor, "create native write parent")?;
            }
            fs::write(path, content)
                .replay_context(ReplayErrorKind::Executor, "write native tool file")?;
            Ok((Value::String(String::new()), Some(0)))
        }
        "edit" => {
            let path = tool_path(
                arguments
                    .get("path")
                    .or_else(|| arguments.get("filePath"))
                    .or_else(|| arguments.get("file_path")),
                context,
            )?;
            let old = arguments
                .get("oldString")
                .or_else(|| arguments.get("old_str"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let new = arguments
                .get("newString")
                .or_else(|| arguments.get("new_str"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let original = String::from_utf8_lossy(&read_regular_file(&path)?).into_owned();
            if !original.contains(old) {
                return Err(ReplayError::trajectory(format!(
                    "edit target does not contain old text: {}",
                    path.display()
                )));
            }
            fs::write(&path, original.replacen(old, new, 1))
                .replay_context(ReplayErrorKind::Executor, "write native edit")?;
            Ok((Value::String(String::new()), Some(0)))
        }
        _ => Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!(
                "{} replay does not support tool {name:?}; use a command-shaped tool or add an adapter",
                context.request.agent.as_str()
            ),
        )),
    }
}

fn normalized_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn tool_path(value: Option<&Value>, context: &RunContext<'_>) -> Result<PathBuf, ReplayError> {
    let rendered = value
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("native file tool has no path"))?;
    let path = Path::new(rendered);
    let workspace = canonicalize(
        &context.request.workspace,
        ReplayErrorKind::Workspace,
        "workspace",
    )?;
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    // Resolve the nearest existing ancestor when the target is new. This
    // catches a symlinked directory that would otherwise let a write escape
    // the workspace before the file itself exists.
    let check = if fs::symlink_metadata(&candidate).is_err() {
        let mut existing = candidate.as_path();
        let mut missing = Vec::new();
        while fs::symlink_metadata(existing).is_err() {
            missing.push(
                existing
                    .file_name()
                    .ok_or_else(|| ReplayError::trajectory("native file tool path has no name"))?
                    .to_os_string(),
            );
            existing = existing.parent().ok_or_else(|| {
                ReplayError::trajectory("native file tool path has no existing parent")
            })?;
        }
        let mut resolved = canonicalize(
            existing,
            ReplayErrorKind::Executor,
            "native file tool path parent",
        )?;
        for component in missing.iter().rev() {
            resolved.push(component);
        }
        resolved
    } else {
        canonicalize(
            &candidate,
            ReplayErrorKind::Executor,
            "native file tool path",
        )?
    };
    if !check.starts_with(&workspace) {
        return Err(ReplayError::trajectory(format!(
            "native file tool path escapes workspace: {rendered:?}"
        )));
    }
    Ok(candidate)
}

fn replace_opencode_observation(
    event: &mut Value,
    fresh: &FreshObservation,
) -> Result<(), ReplayError> {
    let part = event
        .get_mut("part")
        .ok_or_else(|| ReplayError::trajectory("OpenCode tool event has no part"))?;
    let state = part
        .as_object_mut()
        .and_then(|part| part.get_mut("state"))
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ReplayError::trajectory("OpenCode tool event has no state"))?;
    state.insert(
        "status".into(),
        json!(if fresh.is_error { "error" } else { "completed" }),
    );
    state.insert("output".into(), fresh.content.clone());
    if fresh.is_error {
        state.insert("error".into(), fresh.content.clone());
    } else {
        state.remove("error");
    }
    Ok(())
}

fn replace_codex_observation(
    event: &mut Value,
    fresh: &FreshObservation,
) -> Result<(), ReplayError> {
    let payload = event
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| ReplayError::trajectory("Codex output event has no payload"))?;
    payload.insert("output".into(), fresh.content.clone());
    if fresh.is_error {
        payload.insert("status".into(), json!("failed"));
    }
    Ok(())
}

fn write_jsonl(path: &Path, values: &[Value]) -> Result<(), ReplayError> {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value)
            .replay_context(ReplayErrorKind::Executor, "serialize native JSONL")?;
        bytes.push(b'\n');
    }
    atomic_write(path, &bytes)
}

fn write_comparison(
    context: &RunContext<'_>,
    plan: &ReplayPlan,
    observations: &[FreshObservation],
) -> Result<(), ReplayError> {
    let comparisons = plan.calls().zip(observations).map(|(call, fresh)| json!({
        "call_id": call.call_id,
        "tool": call.name,
        "exact": call.original_observation == fresh.content && call.original_is_error == fresh.is_error,
        "original_is_error": call.original_is_error,
        "replayed_is_error": fresh.is_error,
    })).collect::<Vec<_>>();
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )
}

fn continue_native_cli(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    agent: NativeJsonlAgent,
    prefix: &[Value],
    reconstructed: &Path,
) -> Result<(PathBuf, usize), ReplayError> {
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("native CLI replay has no launch spec"))?;
    let logs = context.output_dir.join("logs");
    fs::create_dir_all(&logs)
        .replay_context(ReplayErrorKind::Executor, "create native CLI log directory")?;
    let log_path = logs.join(format!("{}.log", agent.label()));
    // `PlaybackRequest::session_id` is the pVisor/model-router session.  It
    // must not be used as a native Codex session identity: SweEval (and other
    // callers) deliberately set it to a routing key such as
    // `sweeval-<hash>`.  Codex resume resolves the rollout from the native
    // session id stored in the source trajectory.  Mixing the two makes
    // `codex exec resume` silently start a fresh conversation, which is much
    // worse than failing the replay because the resulting patch can still
    // pass a verifier while A(N+1) is no longer comparable with A'(N+1).
    let session_id = continuation_session_id(agent, plan, context)?;
    let mut command = agent_command(&launch.entrypoint, context);
    let mut codex_bridge = None;
    let mut codex_transport_prompt = None;
    let mut codex_prompt_mode = None;
    command.env("PVISOR_REPLAY_TRAJECTORY", reconstructed);
    command.env("PVISOR_REPLAY_AFTER_STEP", plan.after_step.to_string());
    command.env(
        "PVISOR_REPLAY_MAX_STEPS",
        context
            .request
            .max_steps
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    command.env("PVISOR_REPLAY_SESSION_ID", &session_id);
    match agent {
        NativeJsonlAgent::Opencode => {
            let session_id = opencode_session_id(&session_id);
            // The CLI refuses `run --session` without a message, and any
            // message would contaminate the boundary. Continue through the
            // server API instead; see opencode_server_continuation.
            let _ = &command;
            return opencode_server_continuation(plan, context, journal, prefix, &session_id, &log_path);
        }
        NativeJsonlAgent::Codex => {
            let explicit_prompt = context.request.boundary_user_prompt().map(str::to_owned);
            let transport_prompt = explicit_prompt
                .clone()
                .unwrap_or_else(|| format!("pvisor-codex-resume-{}", context.nonce));
            let bridge = CodexBridgeHandle::start(
                context.session_id,
                transport_prompt.clone(),
                explicit_prompt.as_deref(),
            )?;
            let codex_home = context.state_dir.join("codex-home");
            let session_path = codex_session_path(&codex_home, &session_id, &plan.native)?;
            fs::create_dir_all(
                session_path
                    .parent()
                    .expect("Codex session path has parent"),
            )
            .replay_context(ReplayErrorKind::Executor, "create Codex session directory")?;
            let staged = codex_staged_events(prefix, &session_id, &context.request.workspace);
            write_jsonl(&session_path, &staged)?;
            // Recent Codex releases resolve the Responses endpoint from a
            // model-provider profile.  Point the isolated profile at the
            // local SandboxReplay bridge; the bridge removes the transport
            // nonce before forwarding the request upstream.
            let encoded = serde_json::to_string(bridge.base_url.as_str()).map_err(|error| {
                ReplayError::configuration(format!("cannot encode Codex bridge URL: {error}"))
            })?;
            let config = format!(
                "model_provider = \"pvisor-replay\"\n\n[model_providers.pvisor-replay]\nname = \"pvisor-replay\"\nbase_url = {encoded}\nenv_key = \"OPENAI_API_KEY\"\nwire_api = \"responses\"\n"
            );
            atomic_write(&codex_home.join("config.toml"), config.as_bytes())?;
            for (name, value) in bridge.child_environment() {
                command.env(name, value);
            }
            command.env("CODEX_HOME", &codex_home);
            command.args([
                "exec",
                "resume",
                &session_id,
                "--json",
                "--skip-git-repo-check",
                "--dangerously-bypass-approvals-and-sandbox",
            ]);
            // ``exec resume`` otherwise falls back to Codex's default model.
            // The native SweEval launch pins the configured model explicitly;
            // carry the same value into the continuation so local
            // OpenAI-compatible endpoints do not receive an unsupported
            // default model (for example ``gpt-5``).
            if let Some(model) = configured_model_from_environment() {
                command.args(["--model", model.rsplit('/').next().unwrap_or(&model)]);
            }
            if let Some(reasoning_effort) = std::env::var("PVISOR_REPLAY_REASONING_EFFORT")
                .ok()
                .filter(|value| !value.trim().is_empty())
            {
                command.args(["-c", &format!("model_reasoning_effort={reasoning_effort}")]);
            }
            if let Some(max_steps) = context.request.max_steps {
                command.args(["-c", &format!("agent_max_steps={max_steps}")]);
            }
            // Older Codex releases require a prompt argument for resume.  In
            // the default mode this is an opaque nonce removed by the local
            // Responses bridge before the request reaches the model.
            command.arg(&transport_prompt);
            codex_transport_prompt = Some(transport_prompt);
            codex_prompt_mode = Some(bridge.prompt_mode());
            codex_bridge = Some(bridge);
        }
    }
    journal.append(
        "continuation_started",
        [("agent".into(), json!(agent.label()))],
    )?;
    let output = run_process(ProcessSpec {
        command,
        stdin: None,
        timeout: Duration::from_secs(24 * 60 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log_path.clone(),
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    let bridge_result = codex_bridge.take().map(|bridge| bridge.finish());
    let bridge_error = bridge_result.and_then(|result| result.err());
    if !output.status.success() {
        let process_error = ReplayError::classify_continuation(
            format!(
                "{} replay/continuation exited {}; see {}",
                agent.label(),
                output.status,
                log_path.display()
            ),
            &String::from_utf8_lossy(&output.stderr_tail),
        );
        if let Some(error) = bridge_error {
            return Err(ReplayError::continuation(format!(
                "{process_error}; Codex bridge validation also failed: {error}"
            )));
        }
        return Err(process_error);
    }
    if let Some(error) = bridge_error {
        return Err(error);
    }
    let output_path = context.output_dir.join("native/continued-trajectory.jsonl");
    let (continued_events, continued_steps) = match agent {
        NativeJsonlAgent::Codex => {
            let codex_home = context.state_dir.join("codex-home");
            let staged_path = codex_session_path(&codex_home, &session_id, &plan.native)?;
            // `exec resume` may rotate the rollout into a new timestamped
            // file instead of appending to the staged path.  Prefer the
            // newest file from this isolated CODEX_HOME so the continuation
            // is not reported as zero-step merely because we read the stale
            // prefix file.
            let session_path =
                latest_codex_session_path(&codex_home, session_id.as_str()).unwrap_or(staged_path);
            let raw_events = if session_path.is_file() {
                parse_jsonl(&read_regular_file(&session_path)?, agent.label())?
            } else {
                Vec::new()
            };
            validate_codex_continuation(&raw_events, plan, &session_id, &session_path)?;
            let events = clean_codex_transport_events(
                raw_events,
                plan,
                &session_id,
                codex_prompt_mode.unwrap_or(PromptMode::TransportNonce),
                codex_transport_prompt.as_deref(),
                &session_path,
            )?;
            validate_codex_continuation(&events, plan, &session_id, &session_path)?;
            let steps = count_codex_turns_after(&events, plan);
            (events, steps)
        }
        NativeJsonlAgent::Opencode => {
            let raw = read_regular_file(&log_path)?;
            let events = parse_json_lines_from_log(&raw);
            let steps = count_opencode_turns(&events);
            let mut combined = prefix.to_vec();
            combined.extend(events);
            (combined, steps)
        }
    };
    if continued_events.is_empty() {
        return Err(ReplayError::continuation(
            "native CLI produced no JSONL trajectory",
        ));
    }
    write_jsonl(&output_path, &continued_events)?;
    Ok((output_path, continued_steps))
}

fn configured_model_from_environment() -> Option<String> {
    std::env::var("MODEL_NAME")
        .ok()
        .or_else(|| std::env::var("OPENAI_MODEL").ok())
        .filter(|model| !model.trim().is_empty())
}

fn env_f64(name: &str) -> Option<f64> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
}

/// Provider config for the isolated continuation `XDG_CONFIG_HOME`.
///
/// OpenCode reads the endpoint from `OPENAI_BASE_URL`, but sampling options
/// have no environment channel, so a live continuation would silently fall
/// back to provider defaults and diverge from the recorded sampling. The
/// shape mirrors what a SweEval trial writes for the original run.
/// `effective_base` overrides the environment endpoint (used for the local
/// sampling-injection proxy).
fn opencode_provider_config(
    model: &str,
    base_url: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> Option<Value> {
    let (provider, model_id) = model.split_once('/')?;
    let base_url = base_url.map(str::trim).filter(|value| !value.is_empty());
    if base_url.is_none() && temperature.is_none() && top_p.is_none() {
        return None;
    }
    let mut provider_config = serde_json::Map::new();
    if let Some(base_url) = base_url {
        provider_config.insert("options".into(), json!({ "baseURL": base_url }));
    }
    if temperature.is_some() || top_p.is_some() {
        let mut model_options = serde_json::Map::new();
        if let Some(temperature) = temperature {
            model_options.insert("temperature".into(), json!(temperature));
        }
        if let Some(top_p) = top_p {
            model_options.insert("topP".into(), json!(top_p));
        }
        provider_config.insert(
            "models".into(),
            json!({ model_id: { "options": Value::Object(model_options) } }),
        );
    }
    Some(json!({ "provider": { provider: Value::Object(provider_config) } }))
}

fn write_opencode_provider_config(
    config_root: &Path,
    effective_base: Option<&str>,
    temperature: Option<f64>,
    top_p: Option<f64>,
) -> Result<(), ReplayError> {
    let Some(model) = configured_model_from_environment() else {
        return Ok(());
    };
    let base_url = match effective_base {
        Some(base) => Some(base.to_owned()),
        None => std::env::var("OPENAI_BASE_URL")
            .ok()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok()),
    };
    let config = opencode_provider_config(
        &model,
        base_url.as_deref(),
        temperature,
        top_p,
    );
    let Some(config) = config else {
        return Ok(());
    };
    let directory = config_root.join("opencode");
    fs::create_dir_all(&directory)
        .replay_context(ReplayErrorKind::Executor, "create OpenCode config directory")?;
    atomic_write_json(&directory.join("opencode.json"), &config)
}

/// Node script that forwards requests to the model endpoint and injects
/// sampling parameters into JSON bodies. OpenCode 1.17.7 never forwards
/// `temperature`/`top_p` to the Responses API regardless of configuration,
/// so pinning sampling has to happen on the wire. Node comes from the same
/// runtime tree as the OpenCode entrypoint.
fn opencode_sampling_proxy_script() -> &'static str {
    r#"const http = require("node:http")
const https = require("node:https")
const fs = require("node:fs")
const upstream = new URL(process.env.PVISOR_PROXY_UPSTREAM)
const temperature = Number(process.env.PVISOR_PROXY_TEMPERATURE)
const topP = process.env.PVISOR_PROXY_TOP_P === "" ? null : Number(process.env.PVISOR_PROXY_TOP_P)
const server = http.createServer((request, response) => {
  const chunks = []
  request.on("data", (chunk) => chunks.push(chunk))
  request.on("end", () => {
    let body = Buffer.concat(chunks)
    const headers = {...request.headers}
    delete headers["content-length"]
    delete headers["transfer-encoding"]
    if (String(headers["content-type"] || "").includes("application/json") && body.length > 0) {
      try {
        const document = JSON.parse(body.toString("utf8"))
        if (document && typeof document === "object" && !Array.isArray(document)) {
          document.temperature = temperature
          if (topP !== null) document.top_p = topP
          body = Buffer.from(JSON.stringify(document))
        }
      } catch (error) { /* forward the original body */ }
    }
    const target = new URL(upstream)
    target.pathname = request.url
    const transport = target.protocol === "https:" ? https : http
    const forwarded = transport.request(target, {method: request.method, headers}, (up) => {
      response.writeHead(up.statusCode, up.headers)
      up.pipe(response)
    })
    forwarded.on("error", (error) => {
      if (!response.headersSent) response.writeHead(502)
      if (!response.writableEnded) response.end(String(error))
    })
    forwarded.end(body)
  })
})
server.listen(Number(process.env.PVISOR_PROXY_PORT), "127.0.0.1", () => {
  fs.writeFileSync(process.env.PVISOR_PROXY_READY, "ready")
})
"#
}

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn loopback_free_port() -> Result<u16, ReplayError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .replay_context(ReplayErrorKind::Executor, "allocate loopback port")?;
    let port = listener
        .local_addr()
        .replay_context(ReplayErrorKind::Executor, "read loopback port")?
        .port();
    Ok(port)
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<(), ReplayError> {
    let started = Instant::now();
    while !path.is_file() {
        if started.elapsed() > timeout {
            return Err(ReplayError::continuation(format!(
                "OpenCode helper did not become ready: {}",
                path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Start the sampling-injection proxy when sampling overrides are requested.
/// Returns the OpenAI-style API base URL OpenCode should use instead of the
/// real endpoint.
fn start_opencode_sampling_proxy(
    entrypoint: &Path,
    state_dir: &Path,
    logs: &Path,
    upstream: &str,
    temperature: Option<f64>,
    top_p: Option<f64>,
    guard: &mut Vec<ChildGuard>,
) -> Result<Option<String>, ReplayError> {
    if temperature.is_none() && top_p.is_none() {
        return Ok(None);
    }
    let node = {
        let sibling = entrypoint
            .parent()
            .map(|parent| parent.join("node"))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| PathBuf::from("node"));
        sibling
    };
    let port = loopback_free_port()?;
    let upstream = upstream.trim_end_matches('/').to_owned();
    let ready = state_dir.join("opencode-sampling-proxy.ready");
    let _ = fs::remove_file(&ready);
    let script = state_dir.join("opencode-sampling-proxy.js");
    atomic_write(&script, opencode_sampling_proxy_script().as_bytes())?;
    let log = fs::File::create(logs.join("opencode-sampling-proxy.log"))
        .replay_context(ReplayErrorKind::Executor, "create sampling proxy log")?;
    let child = Command::new(&node)
        .arg(&script)
        .env("PVISOR_PROXY_UPSTREAM", &upstream)
        .env(
            "PVISOR_PROXY_TEMPERATURE",
            temperature.map(|v| v.to_string()).unwrap_or_default(),
        )
        .env(
            "PVISOR_PROXY_TOP_P",
            top_p.map(|v| v.to_string()).unwrap_or_default(),
        )
        .env("PVISOR_PROXY_PORT", port.to_string())
        .env("PVISOR_PROXY_READY", &ready)
        .stdout(log.try_clone().replay_context(
            ReplayErrorKind::Executor,
            "clone sampling proxy log handle",
        )?)
        .stderr(log)
        .spawn()
        .replay_context(ReplayErrorKind::Executor, "start OpenCode sampling proxy")?;
    guard.push(ChildGuard(child));
    wait_for_file(&ready, Duration::from_secs(30))?;
    Ok(Some(format!("http://127.0.0.1:{port}/v1")))
}

/// Project the assistant messages created by the live continuation into the
/// native OpenCode JSONL event stream. One assistant message is one agent
/// step; its parts are stored unordered, so restore the canonical order:
/// step-start first, step-finish last, content parts by their start time.
fn opencode_project_messages(messages: &[Value], session_id: &str) -> Vec<Value> {
    let mut ordered: Vec<&Value> = messages
        .iter()
        .filter(|message| message.get("info").and_then(|i| i.get("role")) == Some(&json!("assistant")))
        .collect();
    ordered.sort_by_key(|message| {
        message
            .pointer("/info/time/created")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    let mut events = Vec::new();
    for message in ordered {
        let mut parts: Vec<&Value> = message
            .get("parts")
            .and_then(Value::as_array)
            .map(|parts| parts.iter().collect())
            .unwrap_or_default();
        parts.sort_by(|a, b| opencode_part_order(a).cmp(&opencode_part_order(b)));
        for part in parts {
            let event_type = match part.get("type").and_then(Value::as_str) {
                Some("step-start") => Some("step_start"),
                Some("text") => Some("text"),
                Some("reasoning") => Some("reasoning"),
                Some("tool") => Some("tool_use"),
                Some("step-finish") => Some("step_finish"),
                _ => None,
            };
            let Some(event_type) = event_type else {
                continue;
            };
            events.push(json!({
                "type": event_type,
                "sessionID": session_id,
                "part": part,
            }));
        }
    }
    events
}

fn opencode_part_order(part: &Value) -> (u8, u64) {
    let rank = match part.get("type").and_then(Value::as_str) {
        Some("step-start") => 0,
        Some("step-finish") => 2,
        _ => 1,
    };
    let start = part.pointer("/time/start").and_then(Value::as_u64).unwrap_or(0);
    (rank, start)
}

/// Continue an imported OpenCode session without adding any user message.
///
/// `opencode run --session <id>` refuses to start without a message
/// ("You must provide a message or a command"), so the CLI cannot express a
/// clean continuation. The server API accepts `parts: []` on
/// `POST /session/{id}/message`; the resulting model request ends exactly at
/// the replayed boundary observation `O'N` with no injected prompt.
fn opencode_server_continuation(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    prefix: &[Value],
    session_id: &str,
    log_path: &Path,
) -> Result<(PathBuf, usize), ReplayError> {
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("OpenCode continuation has no launch spec"))?;
    let state_dir = context.state_dir;
    let opencode_config = state_dir.join("opencode-config");
    let opencode_data = state_dir.join("opencode-data");
    fs::create_dir_all(&opencode_data)
        .replay_context(ReplayErrorKind::Executor, "create OpenCode data directory")?;
    let logs = log_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

    let temperature = env_f64("PVISOR_OPENCODE_TEMPERATURE");
    let top_p = env_f64("PVISOR_OPENCODE_TOP_P");
    let mut children: Vec<ChildGuard> = Vec::new();
    let result = opencode_server_continuation_inner(
        plan,
        context,
        journal,
        prefix,
        session_id,
        log_path,
        &launch.entrypoint,
        &opencode_config,
        &opencode_data,
        &logs,
        temperature,
        top_p,
        &mut children,
    );
    drop(children);
    result
}

#[allow(clippy::too_many_arguments)]
fn opencode_server_continuation_inner(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    prefix: &[Value],
    session_id: &str,
    log_path: &Path,
    entrypoint: &Path,
    opencode_config: &Path,
    opencode_data: &Path,
    logs: &Path,
    temperature: Option<f64>,
    top_p: Option<f64>,
    children: &mut Vec<ChildGuard>,
) -> Result<(PathBuf, usize), ReplayError> {
    let workspace = &context.request.workspace;
    let upstream = std::env::var("OPENAI_BASE_URL")
        .ok()
        .or_else(|| std::env::var("OPENAI_API_BASE").ok())
        .filter(|value| !value.trim().is_empty());
    let effective_base = start_opencode_sampling_proxy(
        entrypoint,
        context.state_dir,
        logs,
        upstream.as_deref().unwrap_or("http://127.0.0.1"),
        temperature,
        top_p,
        children,
    )?;
    write_opencode_provider_config(opencode_config, effective_base.as_deref(), temperature, top_p)?;

    let export_path = context.output_dir.join("native/opencode-session.json");
    atomic_write_json(
        &export_path,
        &opencode_export(plan, prefix, session_id, workspace),
    )?;
    let mut import = Command::new(entrypoint);
    import
        .arg("import")
        .arg(
            export_path
                .to_str()
                .ok_or_else(|| ReplayError::configuration("OpenCode export path is not valid UTF-8"))?,
        )
        .env("XDG_CONFIG_HOME", opencode_config)
        .env("XDG_DATA_HOME", opencode_data)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .current_dir(workspace);
    let import_log = logs.join("opencode-import.log");
    let imported = run_process(ProcessSpec {
        command: import,
        stdin: None,
        timeout: Duration::from_secs(5 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 4,
        log_path: import_log.clone(),
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    if !imported.status.success() {
        return Err(ReplayError::classify_continuation(
            format!(
                "OpenCode session import exited {}; see {}",
                imported.status,
                import_log.display()
            ),
            &String::from_utf8_lossy(&imported.stderr_tail),
        ));
    }

    journal.append(
        "continuation_started",
        [("agent".into(), json!("opencode"))],
    )?;

    let serve_port = loopback_free_port()?;
    let serve_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .replay_context(ReplayErrorKind::Executor, "open OpenCode serve log")?;
    let mut serve = Command::new(entrypoint);
    serve
        .args(["serve", "--port", &serve_port.to_string(), "--hostname", "127.0.0.1"])
        .env("XDG_CONFIG_HOME", opencode_config)
        .env("XDG_DATA_HOME", opencode_data)
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .current_dir(workspace)
        .stdout(serve_log.try_clone().replay_context(
            ReplayErrorKind::Executor,
            "clone OpenCode serve log handle",
        )?)
        .stderr(serve_log);
    let serve = serve
        .spawn()
        .replay_context(ReplayErrorKind::Executor, "start OpenCode server")?;
    children.push(ChildGuard(serve));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .replay_context(ReplayErrorKind::Executor, "build OpenCode continuation runtime")?;
    let base = format!("http://127.0.0.1:{serve_port}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .replay_context(ReplayErrorKind::Executor, "build OpenCode continuation client")?;
    let directory = workspace.to_string_lossy().to_string();

    let ready = runtime.block_on(async {
        for _ in 0..120 {
            if let Ok(response) = client
                .get(format!("{base}/config"))
                .query(&[("directory", directory.as_str())])
                .send()
                .await
            {
                if response.status().is_success() {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(ReplayError::continuation(
            "OpenCode server did not become ready; see opencode.log",
        ))
    });
    ready?;

    let messages_url = format!("{base}/session/{session_id}/message");
    let baseline: Vec<String> = runtime
        .block_on(async {
            let response = client
                .get(&messages_url)
                .query(&[("directory", directory.as_str())])
                .send()
                .await
                .replay_context(ReplayErrorKind::Continuation, "read imported session messages")?
                .error_for_status()
                .replay_context(ReplayErrorKind::Continuation, "imported session request failed")?;
            let messages: Value = response
                .json()
                .await
                .replay_context(ReplayErrorKind::Continuation, "decode imported session messages")?;
            Ok(message_ids(&messages))
        })?;

    let mut prompt_body = json!({"parts": []});
    if let Some(model) = configured_model_from_environment() {
        if let Some((provider, model_id)) = model.split_once('/') {
            prompt_body["model"] = json!({"providerID": provider, "modelID": model_id});
        }
    }
    let prompt_response: Value = runtime.block_on(async {
        let response = client
            .post(&messages_url)
            .query(&[("directory", directory.as_str())])
            .json(&prompt_body)
            .send()
            .await
            .replay_context(ReplayErrorKind::Continuation, "send OpenCode continuation prompt")?;
        let status = response.status();
        let payload: Value = response
            .json()
            .await
            .replay_context(ReplayErrorKind::Continuation, "decode OpenCode continuation response")?;
        if !status.is_success() {
            return Err(ReplayError::classify_continuation(
                format!("OpenCode continuation returned {status}; see {}", log_path.display()),
                &payload.to_string(),
            ));
        }
        if let Some(error) = payload.get("error") {
            return Err(ReplayError::classify_continuation(
                format!("OpenCode continuation failed; see {}", log_path.display()),
                &error.to_string(),
            ));
        }
        Ok(payload)
    })?;
    let _ = prompt_response; // final assistant message; the projection re-reads the session

    let messages: Value = runtime.block_on(async {
        let response = client
            .get(&messages_url)
            .query(&[("directory", directory.as_str())])
            .send()
            .await
            .replay_context(ReplayErrorKind::Continuation, "read continued session messages")?
            .error_for_status()
            .replay_context(ReplayErrorKind::Continuation, "continued session request failed")?;
        response
            .json()
            .await
            .replay_context(ReplayErrorKind::Continuation, "decode continued session messages")
    })?;
    let fresh: Vec<Value> = as_message_array(&messages)
        .into_iter()
        .filter(|message| {
            message
                .get("info")
                .and_then(|info| info.get("id"))
                .and_then(Value::as_str)
                .is_some_and(|id| !baseline.contains(&id.to_owned()))
        })
        .collect();
    let live_events = opencode_project_messages(&fresh, session_id);
    let continued_steps = live_events
        .iter()
        .filter(|event| event.get("type") == Some(&json!("step_finish")))
        .count();
    if live_events.is_empty() {
        return Err(ReplayError::continuation(
            "OpenCode continuation produced no native events; see opencode.log",
        ));
    }
    let mut combined = prefix.to_vec();
    combined.extend(live_events);
    let output_path = context.output_dir.join("native/continued-trajectory.jsonl");
    write_jsonl(&output_path, &combined)?;
    Ok((output_path, continued_steps))
}

fn message_ids(messages: &Value) -> Vec<String> {
    as_message_array(messages)
        .iter()
        .filter_map(|message| {
            message
                .get("info")
                .and_then(|info| info.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn as_message_array(messages: &Value) -> Vec<&Value> {
    let items = match messages {
        Value::Array(items) => Some(items.iter().collect()),
        Value::Object(_) => messages
            .get("data")
            .and_then(Value::as_array)
            .map(|items| items.iter().collect()),
        _ => None,
    };
    items.unwrap_or_default()
}

fn continuation_session_id(
    agent: NativeJsonlAgent,
    plan: &ReplayPlan,
    context: &RunContext<'_>,
) -> Result<String, ReplayError> {
    let native = plan
        .native
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    match agent {
        NativeJsonlAgent::Codex => codex_native_session_id(&plan.native),
        NativeJsonlAgent::Opencode => Ok(context
            .request
            .session_id
            .as_deref()
            .or(native)
            .unwrap_or(context.session_id)
            .to_owned()),
    }
}

fn codex_native_session_id(native: &Value) -> Result<String, ReplayError> {
    native
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            ReplayError::continuation(
                "Codex trajectory has no native session_meta id; refusing to use the pVisor/router session_id for resume",
            )
        })
}

fn latest_codex_session_path(root: &Path, session_id: &str) -> Option<PathBuf> {
    fn visit(
        directory: &Path,
        session_id: &str,
        newest: &mut Option<(std::time::SystemTime, PathBuf)>,
    ) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, session_id, newest);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
                continue;
            }
            // Never pick an unrelated/stale rollout from the isolated
            // CODEX_HOME.  The native session id is also checked from the
            // event stream below; matching the filename avoids selecting a
            // rotated file belonging to another replay attempt.
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !file_name.ends_with(&format!("-{session_id}.jsonl")) {
                continue;
            }
            let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            if newest
                .as_ref()
                .is_none_or(|(current, _)| modified > *current)
            {
                *newest = Some((modified, path));
            }
        }
    }
    let mut newest = None;
    visit(root, session_id, &mut newest);
    newest.map(|(_, path)| path)
}

fn validate_codex_continuation(
    events: &[Value],
    plan: &ReplayPlan,
    session_id: &str,
    path: &Path,
) -> Result<(), ReplayError> {
    if events.is_empty() {
        return Err(ReplayError::continuation(format!(
            "Codex resume produced no events in {}",
            path.display()
        )));
    }
    let observed_session_id = events.iter().find_map(|event| {
        (event.get("type").and_then(Value::as_str) == Some("session_meta")).then(|| {
            event
                .pointer("/payload/id")
                .or_else(|| event.pointer("/payload/session_id"))
                .and_then(Value::as_str)
        })
    });
    if observed_session_id.flatten() != Some(session_id) {
        return Err(ReplayError::continuation(format!(
            "Codex resume did not return native session {session_id:?} (observed {:?}); refusing an unverified continuation",
            observed_session_id.flatten()
        )));
    }

    let assistant_turns = events
        .iter()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response_item")
                && event.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                && event.pointer("/payload/role").and_then(Value::as_str) == Some("assistant")
        })
        .count();
    if assistant_turns < plan.batches.len() {
        return Err(ReplayError::continuation(format!(
            "Codex resume returned only {assistant_turns} assistant turns, but the staged boundary contains {} tool batches; refusing a fresh-session continuation",
            plan.batches.len()
        )));
    }

    if let Some(expected_prompt) = plan
        .native
        .get("user_prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let has_original_prompt = events.iter().any(|event| {
            event.get("type").and_then(Value::as_str) == Some("response_item")
                && event.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                && event.pointer("/payload/role").and_then(Value::as_str) == Some("user")
                && event
                    .get("payload")
                    .and_then(|payload| codex_message_text(payload, "input_text"))
                    .as_deref()
                    == Some(expected_prompt)
        });
        if !has_original_prompt {
            return Err(ReplayError::continuation(
                "Codex resume did not contain the original user task; refusing a fresh-session continuation",
            ));
        }
    }

    if let Some(last_call_id) = plan
        .batches
        .last()
        .and_then(|batch| batch.tool_calls.last())
        .map(|call| call.call_id.as_str())
    {
        let has_boundary_call = events.iter().any(|event| {
            event.pointer("/payload/call_id").and_then(Value::as_str) == Some(last_call_id)
        });
        if !has_boundary_call {
            return Err(ReplayError::continuation(format!(
                "Codex resume did not contain the boundary call {last_call_id:?}; refusing an unverified continuation"
            )));
        }
    }
    Ok(())
}

fn clean_codex_transport_events(
    mut events: Vec<Value>,
    plan: &ReplayPlan,
    _session_id: &str,
    prompt_mode: PromptMode,
    transport_prompt: Option<&str>,
    path: &Path,
) -> Result<Vec<Value>, ReplayError> {
    if prompt_mode == PromptMode::ExplicitUserPrompt {
        return Ok(events);
    }
    let expected = transport_prompt
        .filter(|prompt| !prompt.is_empty())
        .ok_or_else(|| ReplayError::continuation("Codex transport nonce is missing"))?;
    let boundary_call_id = plan
        .batches
        .last()
        .and_then(|batch| batch.tool_calls.last())
        .map(|call| call.call_id.as_str())
        .ok_or_else(|| ReplayError::trajectory("Codex plan has no boundary call"))?;
    let boundary_index = events
        .iter()
        .rposition(|event| {
            event.pointer("/payload/call_id").and_then(Value::as_str) == Some(boundary_call_id)
        })
        .ok_or_else(|| {
            ReplayError::continuation(format!(
                "Codex continuation has no boundary call in {}",
                path.display()
            ))
        })?;
    let matches = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| {
            (index > boundary_index
                && event.get("type").and_then(Value::as_str) == Some("response_item")
                && event.pointer("/payload/type").and_then(Value::as_str) == Some("message")
                && event.pointer("/payload/role").and_then(Value::as_str) == Some("user")
                && event
                    .get("payload")
                    .and_then(|payload| codex_message_text(payload, "input_text"))
                    .as_deref()
                    == Some(expected))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(ReplayError::continuation(format!(
            "expected exactly one Codex transport nonce in the resumed trajectory, found {}",
            matches.len()
        )));
    }
    events.remove(matches[0]);
    for event in &mut events {
        redact_codex_transport_nonce(event, expected);
    }
    let legacy_prompt = "Continue from the replay boundary.";
    if events.iter().any(|event| {
        event.get("type").and_then(Value::as_str) == Some("response_item")
            && event.pointer("/payload/type").and_then(Value::as_str) == Some("message")
            && event.pointer("/payload/role").and_then(Value::as_str) == Some("user")
            && event
                .get("payload")
                .and_then(|payload| codex_message_text(payload, "input_text"))
                .as_deref()
                == Some(legacy_prompt)
    }) {
        return Err(ReplayError::continuation(
            "legacy Codex resume prompt remains in the cleaned trajectory",
        ));
    }
    if events
        .iter()
        .any(|event| event.to_string().contains(expected))
    {
        return Err(ReplayError::continuation(
            "Codex transport nonce remains in the cleaned trajectory",
        ));
    }
    Ok(events)
}

fn redact_codex_transport_nonce(value: &mut Value, expected: &str) {
    match value {
        Value::String(text) => {
            if text.contains(expected) {
                *text = text.replace(expected, "");
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_codex_transport_nonce(value, expected);
            }
        }
        Value::Object(fields) => {
            for value in fields.values_mut() {
                redact_codex_transport_nonce(value, expected);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn codex_session_path(
    codex_home: &Path,
    session_id: &str,
    native: &Value,
) -> Result<PathBuf, ReplayError> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(ReplayError::configuration(
            "Codex session_id must contain only ASCII letters, digits, '-' and '_'",
        ));
    }
    // Codex discovers sessions below CODEX_HOME/sessions by the rollout file
    // name.  Keep the directory and timestamp shape used by the native CLI
    // while placing the staged transcript in the replay state directory.  A
    // fixed 1970 path is accepted by some versions but is not a native rollout
    // identity and can make `exec resume` ignore the staged file.
    let timestamp = native
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| {
            events.iter().find_map(|event| {
                (event.get("type").and_then(Value::as_str) == Some("session_meta")).then(|| {
                    event
                        .pointer("/payload/timestamp")
                        .or_else(|| event.pointer("/timestamp"))
                        .and_then(Value::as_str)
                })
            })
        })
        .flatten()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
    let (directory, filename_timestamp) = timestamp
        .map(|value| {
            (
                value.format("%Y/%m/%d").to_string(),
                value.format("%Y-%m-%dT%H-%M-%S").to_string(),
            )
        })
        .unwrap_or_else(|| ("1970/01/01".into(), "1970-01-01T00-00-00".into()));
    Ok(codex_home.join(format!(
        "sessions/{directory}/rollout-{filename_timestamp}-{session_id}.jsonl"
    )))
}

fn opencode_session_id(raw: &str) -> String {
    let suffix = raw
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect::<String>();
    if raw.starts_with("ses_") && raw == suffix && !suffix.is_empty() {
        raw.to_owned()
    } else {
        format!(
            "ses_pvisor_{}",
            if suffix.is_empty() { "replay" } else { &suffix }
        )
    }
}

fn opencode_export(
    plan: &ReplayPlan,
    prefix: &[Value],
    session_id: &str,
    workspace: &Path,
) -> Value {
    let user_id = "msg_pvisor_user";
    let prompt = plan
        .native
        .get("user_prompt")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // OpenCode resolves a session's default model from the last user message
    // metadata when a request does not pin one. The synthetic placeholder
    // must therefore carry the configured model; "pvisor/replay" would poison
    // that fallback with a provider that does not exist.
    let (placeholder_provider, placeholder_model) = configured_model_from_environment()
        .and_then(|model| model.split_once('/').map(|(p, m)| (p.to_owned(), m.to_owned())))
        .unwrap_or_else(|| ("pvisor".to_owned(), "replay".to_owned()));
    let mut messages = vec![json!({
        "info": {
            "id": user_id,
            "sessionID": session_id,
            "role": "user",
            "time": {"created": 0},
            "agent": "build",
            "model": {"providerID": placeholder_provider, "modelID": placeholder_model},
        },
        "parts": [{
            "id": "prt_pvisor_user",
            "sessionID": session_id,
            "messageID": user_id,
            "type": "text",
            "text": prompt,
        }],
    })];
    for batch in &plan.batches {
        let message_id = format!("msg_pvisor_{:04}", batch.ordinal);
        let mut parts = vec![json!({
            "id": format!("prt_pvisor_step_start_{:04}", batch.ordinal),
            "sessionID": session_id,
            "messageID": message_id,
            "type": "step-start",
        })];
        let start_event = batch
            .native
            .get("start_event")
            .and_then(Value::as_u64)
            .unwrap_or_default() as usize;
        let end_event = batch
            .native
            .get("end_event")
            .and_then(Value::as_u64)
            .unwrap_or(start_event as u64) as usize;
        for (reasoning_ordinal, event) in prefix
            .get(start_event..=end_event.min(prefix.len().saturating_sub(1)))
            .into_iter()
            .flatten()
            .filter(|event| event.get("type").and_then(Value::as_str) == Some("reasoning"))
            .enumerate()
        {
            let Some(text) = event
                .get("part")
                .and_then(|part| part.get("text"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            parts.push(json!({
                "id": format!("prt_pvisor_reasoning_{:04}_{:04}", batch.ordinal, reasoning_ordinal + 1),
                "sessionID": session_id,
                "messageID": message_id,
                "type": "reasoning",
                "text": text,
                "time": {"start": 0, "end": 0},
            }));
        }
        if !batch.assistant_text.is_empty() {
            parts.push(json!({
                "id": format!("prt_pvisor_text_{:04}", batch.ordinal),
                "sessionID": session_id,
                "messageID": message_id,
                "type": "text",
                "text": batch.assistant_text,
            }));
        }
        for (ordinal, call) in batch.tool_calls.iter().enumerate() {
            let output_event = call
                .native
                .get("output_event")
                .and_then(Value::as_u64)
                .unwrap_or_default() as usize;
            let state = prefix
                .get(output_event)
                .and_then(|event| event.get("part"))
                .and_then(|part| part.get("state"));
            let fresh_output = state
                .and_then(|state| state.get("output"))
                .map(render_opencode_output)
                .unwrap_or_default();
            let fresh_error = state
                .and_then(|state| state.get("status"))
                .and_then(Value::as_str)
                == Some("error");
            let input = if call.arguments.is_object() {
                call.arguments.clone()
            } else {
                json!({"value": call.arguments})
            };
            let state = if fresh_error {
                json!({
                    "status": "error",
                    "input": input,
                    "error": fresh_output,
                    "metadata": {},
                    "time": {"start": 0, "end": 0},
                })
            } else {
                json!({
                    "status": "completed",
                    "input": input,
                    "output": fresh_output,
                    "title": call.name,
                    "metadata": {},
                    "time": {"start": 0, "end": 0},
                })
            };
            parts.push(json!({
                "id": format!("prt_pvisor_tool_{:04}_{:04}", batch.ordinal, ordinal + 1),
                "sessionID": session_id,
                "messageID": message_id,
                "type": "tool",
                "callID": call.call_id,
                "tool": call.name,
                "state": state,
            }));
        }
        parts.push(json!({
            "id": format!("prt_pvisor_step_finish_{:04}", batch.ordinal),
            "sessionID": session_id,
            "messageID": message_id,
            "type": "step-finish",
            "reason": "tool-calls",
            "cost": 0,
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
        }));
        messages.push(json!({
            "info": {
                "id": message_id,
                "sessionID": session_id,
                "role": "assistant",
                "time": {"created": batch.ordinal as u64, "completed": batch.ordinal as u64},
                "parentID": user_id,
                "modelID": placeholder_model,
                "providerID": placeholder_provider,
                "mode": "build",
                "agent": "build",
                "path": {"cwd": workspace.display().to_string(), "root": workspace.display().to_string()},
                "cost": 0,
                "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
            },
            "parts": parts,
        }));
    }
    json!({
        "info": {
            "id": session_id,
            "slug": "pvisor-replay",
            "projectID": "global",
            "directory": workspace.display().to_string(),
            "path": "",
            "title": if prompt.is_empty() { "pVisor replay" } else { prompt },
            "version": "1",
            "time": {"created": 0, "updated": plan.batches.len() as u64},
        },
        "messages": messages,
    })
}

fn render_opencode_output(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn codex_staged_events(prefix: &[Value], session_id: &str, workspace: &Path) -> Vec<Value> {
    let mut events = prefix.to_vec();
    let mut found_meta = false;
    for event in &mut events {
        if event.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        found_meta = true;
        if let Some(payload) = event.get_mut("payload").and_then(Value::as_object_mut) {
            payload.insert("id".into(), json!(session_id));
            if payload.contains_key("session_id") {
                payload.insert("session_id".into(), json!(session_id));
            }
            payload
                .entry("cwd")
                .or_insert_with(|| json!(workspace.display().to_string()));
            payload
                .entry("cli_version")
                .or_insert_with(|| json!(AgentKind::Codex.supported_version()));
        }
    }
    if !found_meta {
        events.insert(
            0,
            json!({
                "timestamp": "1970-01-01T00:00:00Z",
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": workspace.display().to_string(),
                    "cli_version": AgentKind::Codex.supported_version(),
                },
            }),
        );
    }
    events
}

fn parse_json_lines_from_log(raw: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            matches!(
                event.get("type").and_then(Value::as_str),
                Some("user" | "step_start" | "text" | "reasoning" | "tool_use" | "step_finish")
            )
        })
        .collect()
}

fn count_opencode_turns(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("step_finish"))
        .count()
}

fn count_codex_turns_after(events: &[Value], plan: &ReplayPlan) -> usize {
    let boundary_call = plan
        .batches
        .last()
        .and_then(|batch| batch.tool_calls.last())
        .map(|call| call.call_id.as_str());
    let boundary_index = boundary_call.and_then(|call_id| {
        events.iter().rposition(|event| {
            event.pointer("/payload/call_id").and_then(Value::as_str) == Some(call_id)
        })
    });
    let continuation_events = boundary_index
        .and_then(|index| events.get(index.saturating_add(1)..))
        .unwrap_or(events);
    let turns = continuation_events
        .iter()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response_item")
                && event.pointer("/payload/role").and_then(Value::as_str) == Some("assistant")
        })
        .count();
    if turns > 0 {
        return turns;
    }
    // Some Codex releases emit tool calls without an assistant message for a
    // short continuation.  Such a stream is still a live turn; use the
    // number of post-prefix tool calls as a conservative lower bound rather
    // than incorrectly classifying a successful run as zero-step.
    let continuation_calls = continuation_events
        .iter()
        .filter(|event| {
            event.get("type").and_then(Value::as_str) == Some("response_item")
                && matches!(
                    event.pointer("/payload/type").and_then(Value::as_str),
                    Some("function_call" | "custom_tool_call")
                )
        })
        .count();
    usize::from(continuation_calls > 0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        CallRecord, NativeJsonlAgent, RunContext, TurnRecord, codex_native_session_id,
        continuation_session_id, is_actionable_turn, opencode_provider_config, parse_codex,
        parse_jsonl, parse_opencode, redact_codex_transport_nonce, validate_codex_continuation,
    };
    use crate::model::{AgentKind, PlaybackRequest, ReplayMode, ReplayPlan, ToolBatch, ToolCall};
    use serde_json::{Value, json};

    #[test]
    fn opencode_provider_config_mirrors_recorded_sampling() {
        let config = opencode_provider_config(
            "openai/model-x",
            Some("http://127.0.0.1:8000/v1"),
            Some(0.0),
            Some(1.0),
        )
        .unwrap();
        assert_eq!(
            config,
            json!({
                "provider": {
                    "openai": {
                        "options": {"baseURL": "http://127.0.0.1:8000/v1"},
                        "models": {"model-x": {"options": {"temperature": 0.0, "topP": 1.0}}}
                    }
                }
            })
        );

        // Without sampling overrides the endpoint still comes from the
        // environment, so only the baseURL section is written.
        let base_only = opencode_provider_config("openai/model-x", Some("http://m:1/v1"), None, None)
            .unwrap();
        assert_eq!(
            base_only,
            json!({"provider": {"openai": {"options": {"baseURL": "http://m:1/v1"}}}})
        );

        // Nothing to pin: leave OpenCode on its environment-only defaults.
        assert!(opencode_provider_config("openai/model-x", None, None, None).is_none());
        // A model without a provider namespace cannot be pinned either.
        assert!(opencode_provider_config("model-x", Some("http://m:1/v1"), Some(0.0), None).is_none());
        // Blank endpoints are ignored rather than written.
        assert!(opencode_provider_config("openai/model-x", Some("  "), None, None).is_none());
    }

    #[test]
    fn opencode_events_group_tool_parts_into_complete_turns() {
        let source = [
            json!({"type":"user","sessionID":"ses-test","parts":[{"type":"text","text":"fix it"}]}),
            json!({"type":"step_start","sessionID":"ses-test"}),
            json!({"type":"text","sessionID":"ses-test","part":{"type":"text","text":"Inspecting"}}),
            json!({"type":"tool_use","sessionID":"ses-test","part":{"type":"tool","callID":"call-1","tool":"bash","state":{"status":"completed","input":{"command":"pwd"},"output":"/workspace"}}}),
            json!({"type":"step_finish","sessionID":"ses-test","part":{"reason":"tool-calls"}}),
            json!({"type":"step_start","sessionID":"ses-test"}),
            json!({"type":"text","sessionID":"ses-test","part":{"type":"text","text":"Done"}}),
            json!({"type":"step_finish","sessionID":"ses-test","part":{"reason":"stop"}}),
        ];
        let (turns, prompt, session) = parse_opencode(&source).unwrap();
        assert_eq!(prompt.as_deref(), Some("fix it"));
        assert_eq!(session.as_deref(), Some("ses-test"));
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].calls[0].name, "bash");
        assert_eq!(turns[0].calls[0].observation, "/workspace");
        assert_eq!(turns[1].text, "Done");
    }

    #[test]
    fn reasoning_only_turn_is_not_an_actionable_next_step() {
        let reasoning_only = TurnRecord {
            start_event: 1,
            end_event: 1,
            text: "   ".into(),
            reasoning: "internal planning".into(),
            calls: Vec::new(),
        };
        assert!(!is_actionable_turn(&reasoning_only));

        let visible_text = TurnRecord {
            text: "continue".into(),
            ..reasoning_only.clone()
        };
        assert!(is_actionable_turn(&visible_text));

        let tool_call = TurnRecord {
            calls: vec![CallRecord {
                call_event: 2,
                output_event: 3,
                call_id: "call-1".into(),
                name: "exec_command".into(),
                arguments: json!({"cmd": "pwd"}),
                observation: Value::Null,
                is_error: false,
                complete: false,
            }],
            ..reasoning_only
        };
        assert!(is_actionable_turn(&tool_call));
    }

    #[test]
    fn codex_rollouts_accept_responses_and_custom_tool_calls() {
        let source = [
            json!({"type":"session_meta","payload":{"id":"sess-test"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix it"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Inspecting"}]}}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-1","name":"exec","input":"{\"command\":\"pwd\"}"}}),
            json!({"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-1","output":[{"type":"input_text","text":"/workspace"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}}),
        ];
        let (turns, prompt, session) = parse_codex(&source).unwrap();
        assert_eq!(prompt.as_deref(), Some("fix it"));
        assert_eq!(session.as_deref(), Some("sess-test"));
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].calls[0].name, "exec");
        assert_eq!(turns[0].calls[0].arguments["command"], "pwd");
        assert_eq!(turns[0].calls[0].output_event, 4);
        assert_eq!(turns[1].text, "Done");
    }

    #[test]
    fn codex_transport_nonce_is_redacted_from_model_echoes() {
        let nonce = "pvisor-codex-resume-abc123";
        let mut event = json!({
            "payload": {
                "summary": [{"text": format!("I received {nonce}")}],
                "content": [{"text": format!("{nonce} should not persist")}]
            }
        });
        redact_codex_transport_nonce(&mut event, nonce);
        assert!(!event.to_string().contains(nonce));
        assert_eq!(event["payload"]["summary"][0]["text"], "I received ");
    }

    #[test]
    fn codex_session_meta_accepts_legacy_top_level_id() {
        let source = [
            json!({"type":"session_meta","id":"legacy-session"}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix it"}]}}),
        ];
        let (_, _, session) = parse_codex(&source).unwrap();
        assert_eq!(session.as_deref(), Some("legacy-session"));
    }

    #[test]
    fn parser_tolerates_only_a_truncated_final_jsonl_line() {
        let raw = b"{\"type\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"hi\"}]}\n{";
        let events = parse_jsonl(raw, NativeJsonlAgent::Opencode.label()).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn codex_native_identity_is_taken_from_the_trajectory() {
        let native = json!({"session_id": "native-session"});
        assert_eq!(codex_native_session_id(&native).unwrap(), "native-session");
        assert!(codex_native_session_id(&json!({})).is_err());
    }

    #[test]
    fn codex_native_identity_cannot_be_overridden_by_router_session() {
        let plan = ReplayPlan {
            agent: AgentKind::Codex,
            source_path: PathBuf::from("/trajectory.jsonl"),
            source_sha256: "sha".into(),
            after_step: 1,
            prefix_model_turns: 1,
            native: json!({"session_id": "native-session", "user_prompt": "Original task"}),
            original_next_action: None,
            batches: Vec::new(),
        };
        let request = PlaybackRequest {
            agent: AgentKind::Codex,
            trajectory: PathBuf::from("/trajectory.jsonl"),
            after_step: 1,
            workspace: PathBuf::from("/workspace"),
            state_dir: PathBuf::from("/state"),
            output_dir: PathBuf::from("/output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: Some("sweeval-router-key".into()),
            max_steps: None,
            mode: ReplayMode::ReplayAndContinue,
            allow_stale_observations: false,
            run_id: None,
            disable_thinking: false,
            boundary_user_prompt: None,
        };
        let context = RunContext {
            request: &request,
            state_dir: std::path::Path::new("/state"),
            output_dir: std::path::Path::new("/output"),
            launch: None,
            session_id: "sweeval-router-key",
            nonce: "nonce",
        };
        assert_eq!(
            continuation_session_id(NativeJsonlAgent::Codex, &plan, &context).unwrap(),
            "native-session"
        );
    }

    #[test]
    fn codex_continuation_rejects_a_fresh_session_without_the_staged_prefix() {
        let plan = ReplayPlan {
            agent: AgentKind::Codex,
            source_path: PathBuf::from("/trajectory.jsonl"),
            source_sha256: "sha".into(),
            after_step: 1,
            prefix_model_turns: 1,
            native: json!({"session_id": "native-session", "user_prompt": "Original task"}),
            original_next_action: None,
            batches: vec![ToolBatch {
                ordinal: 1,
                native_locator: "events:0-3".into(),
                assistant_text: "before".into(),
                native: json!({"start_event": 0, "end_event": 3}),
                tool_calls: vec![ToolCall {
                    ordinal: 1,
                    call_id: "boundary-call".into(),
                    name: "exec_command".into(),
                    arguments: json!({"cmd": "true"}),
                    original_observation: json!("old"),
                    original_is_error: false,
                    native: json!({"call_event": 2, "output_event": 3}),
                }],
            }],
        };
        let fresh = vec![
            json!({"type":"session_meta","payload":{"id":"native-session"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}),
        ];
        let error = validate_codex_continuation(
            &fresh,
            &plan,
            "native-session",
            std::path::Path::new("/rollout.jsonl"),
        )
        .unwrap_err();
        assert!(error.message.contains("user task") || error.message.contains("boundary"));

        let resumed = vec![
            json!({"type":"session_meta","payload":{"id":"native-session"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Original task"}]}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"boundary-call"}}),
            json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[]}}),
        ];
        validate_codex_continuation(
            &resumed,
            &plan,
            "native-session",
            std::path::Path::new("/rollout.jsonl"),
        )
        .unwrap();
    }
}
