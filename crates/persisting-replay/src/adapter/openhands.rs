use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    agent_command, check_boundary, prepared_outcome, LaunchSpec, RunContext, MAX_TOOL_OUTPUT_BYTES,
};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, FreshObservation, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan,
    ToolBatch, ToolCall,
};
use crate::process::{run_process, ProcessSpec};

pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_openhands_plan(request).map(AdapterPlan::Openhands)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_openhands(plan, context, journal)
}

fn build_openhands_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let events: Vec<Value> = serde_json::from_slice(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "invalid OpenHands trajectory JSON",
    )?;
    if events.is_empty() {
        return Err(ReplayError::trajectory(
            "OpenHands trajectory must be a non-empty event array",
        ));
    }
    let mut ids = BTreeSet::new();
    for event in &events {
        let id = event_id(event)?;
        if !ids.insert(id) {
            return Err(ReplayError::trajectory(format!(
                "duplicate OpenHands event id {id}"
            )));
        }
    }
    let observations: BTreeMap<i64, &Value> = events
        .iter()
        .filter_map(|event| {
            (event.get("observation").is_some() && !event["observation"].is_null())
                .then(|| {
                    event
                        .get("cause")
                        .and_then(Value::as_i64)
                        .map(|cause| (cause, event))
                })
                .flatten()
        })
        .collect();
    let supported = ["run", "read", "edit", "run_ipython", "think"];
    let mut batches = Vec::new();
    for action in &events {
        let action_name = action.get("action").and_then(Value::as_str);
        if action.get("source").and_then(Value::as_str) != Some("agent")
            || matches!(action_name, None | Some("system" | "finish" | "message"))
        {
            continue;
        }
        let action_name = action_name.unwrap();
        if !supported.contains(&action_name) {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("unsupported OpenHands action {action_name:?}"),
            ));
        }
        let id = event_id(action)?;
        let Some(observation) = observations.get(&id) else {
            break;
        };
        batches.push(ToolBatch {
            ordinal: batches.len() + 1,
            native_locator: format!("event:{id}"),
            tool_calls: vec![ToolCall {
                ordinal: batches.len() + 1,
                call_id: id.to_string(),
                name: action_name.to_owned(),
                arguments: action.get("args").cloned().unwrap_or_else(|| json!({})),
                original_observation: json!({
                    "observation": observation.get("observation"),
                    "message": observation.get("message"),
                    "args": observation.get("args"),
                }),
                original_is_error: observation.get("observation").and_then(Value::as_str)
                    == Some("error"),
                native: action.clone(),
            }],
            assistant_text: action
                .get("args")
                .and_then(|args| args.get("thought"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            native: json!({"observation_id": observation.get("id")}),
        });
    }
    check_boundary(request.after_step, batches.len())?;
    batches.truncate(request.after_step);
    let boundary_id = batches.last().unwrap().tool_calls[0]
        .call_id
        .parse::<i64>()
        .unwrap();
    let initial_user_event = events
        .iter()
        .find(|event| {
            event.get("source").and_then(Value::as_str) == Some("user")
                && event.get("action").and_then(Value::as_str) == Some("message")
                && event.get("id").and_then(Value::as_i64).unwrap_or(i64::MAX) <= boundary_id
        })
        .cloned()
        .ok_or_else(|| {
            ReplayError::trajectory("OpenHands replay has no user message through the boundary")
        })?;
    let original_next_action = events.iter().find_map(|event| {
        let id = event.get("id").and_then(Value::as_i64)?;
        let action = event.get("action").and_then(Value::as_str)?;
        if id <= boundary_id
            || event.get("source").and_then(Value::as_str) != Some("agent")
            || action == "finish"
        {
            return None;
        }
        Some(openhands_action_signature(event))
    });
    Ok(ReplayPlan {
        agent: request.agent,
        source_path: canonicalize(
            &request.trajectory,
            ReplayErrorKind::Trajectory,
            "trajectory",
        )?,
        source_sha256: sha256(&raw),
        after_step: request.after_step,
        prefix_model_turns: request.after_step,
        batches,
        native: json!({"events": events, "initial_user_event": initial_user_event}),
        original_next_action,
    })
}

fn openhands_action_signature(event: &Value) -> Value {
    let response_message = event
        .get("tool_call_metadata")
        .and_then(|metadata| metadata.get("model_response"))
        .and_then(|response| response.get("choices"))
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"));
    let text = response_message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reasoning = response_message
        .and_then(|message| message.get("reasoning_content"))
        .and_then(Value::as_str)
        .or_else(|| {
            response_message.is_none().then(|| {
                event
                    .get("args")
                    .and_then(|args| args.get("thought"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
        })
        .unwrap_or_default();
    json!({
        "text": text,
        "reasoning": reasoning,
        "tools": [{
            "name": event.get("action").and_then(Value::as_str).unwrap_or_default(),
            "arguments": openhands_reconstructed_tool_arguments(event),
        }],
    })
}

fn openhands_reconstructed_tool_metadata(event: &Value) -> Result<Value, ReplayError> {
    let event_id = event_id(event)?;
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("OpenHands replay action has no action"))?;
    let tool_name = match action {
        "run" => "execute_bash",
        "read" | "edit" => "str_replace_editor",
        "run_ipython" => "execute_ipython_cell",
        "think" => "think",
        _ => {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("unsupported OpenHands action {action:?}"),
            ));
        }
    };
    let tool_call_id = format!("sandbox-playback-replay-{event_id}");
    let arguments = openhands_reconstructed_tool_arguments(event);
    let serialized_arguments = serde_json::to_string(&arguments).replay_context(
        ReplayErrorKind::Internal,
        "serialize reconstructed OpenHands tool arguments",
    )?;
    let thought = event
        .get("args")
        .and_then(|args| args.get("thought"))
        .and_then(Value::as_str)
        .filter(|thought| !thought.is_empty())
        .map(str::to_owned);
    Ok(json!({
        "function_name": tool_name,
        "tool_call_id": tool_call_id.clone(),
        "total_calls_in_response": 1,
        "model_response": {
            "id": format!("sandbox-playback-response-{event_id}"),
            "created": 0,
            "model": "sandbox-playback/reconstructed",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": thought,
                    "tool_calls": [{
                        "id": tool_call_id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": serialized_arguments,
                        },
                    }],
                },
            }],
        },
    }))
}

fn openhands_reconstructed_tool_arguments(event: &Value) -> Value {
    let action = event
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source = event.get("args").cloned().unwrap_or_else(|| json!({}));
    match action {
        "run" => {
            let mut arguments = serde_json::Map::from_iter([(
                "command".to_owned(),
                source.get("command").cloned().unwrap_or_else(|| json!("")),
            )]);
            if let Some(value) = source.get("is_input") {
                arguments.insert(
                    "is_input".to_owned(),
                    if let Some(value) = value.as_bool() {
                        Value::String(value.to_string())
                    } else {
                        value.clone()
                    },
                );
            }
            if let Some(value) = source.get("timeout").filter(|value| !value.is_null()) {
                arguments.insert("timeout".to_owned(), value.clone());
            }
            Value::Object(arguments)
        }
        "run_ipython" => json!({
            "code": source.get("code").cloned().unwrap_or_else(|| json!("")),
        }),
        "read" => {
            let mut arguments = serde_json::Map::from_iter([
                ("command".to_owned(), json!("view")),
                (
                    "path".to_owned(),
                    source.get("path").cloned().unwrap_or_else(|| json!("")),
                ),
            ]);
            if let Some(value) = source.get("view_range").filter(|value| !value.is_null()) {
                arguments.insert("view_range".to_owned(), value.clone());
            }
            Value::Object(arguments)
        }
        "edit" => {
            let mut arguments = serde_json::Map::new();
            for key in [
                "command",
                "path",
                "file_text",
                "old_str",
                "new_str",
                "insert_line",
                "view_range",
            ] {
                if let Some(value) = source.get(key) {
                    arguments.insert(key.to_owned(), value.clone());
                }
            }
            arguments
                .entry("command".to_owned())
                .or_insert(json!("str_replace"));
            Value::Object(arguments)
        }
        "think" => json!({
            "thought": source.get("thought").cloned().unwrap_or_else(|| json!("")),
        }),
        _ => source,
    }
}

fn event_id(event: &Value) -> Result<i64, ReplayError> {
    event
        .get("id")
        .and_then(Value::as_i64)
        .ok_or_else(|| ReplayError::trajectory("OpenHands event has no integer id"))
}

fn run_openhands(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let events = plan.native["events"].as_array().unwrap();
    let initial = plan.native["initial_user_event"].clone();
    let boundary_id = plan.batches.last().unwrap().tool_calls[0]
        .call_id
        .parse::<i64>()
        .unwrap();
    let mut prepared_events = vec![initial.clone()];
    for event in events {
        if event_id(event)? > boundary_id {
            break;
        }
        if event == &initial || event.get("action").and_then(Value::as_str) == Some("system") {
            continue;
        }
        if event.get("action").is_some() && !event["action"].is_null() {
            let mut reconstructed = event.clone();
            if reconstructed.get("source").and_then(Value::as_str) == Some("agent")
                && matches!(
                    reconstructed.get("action").and_then(Value::as_str),
                    Some("run" | "read" | "edit" | "run_ipython" | "think")
                )
                && reconstructed
                    .get("tool_call_metadata")
                    .is_none_or(Value::is_null)
            {
                reconstructed["tool_call_metadata"] =
                    openhands_reconstructed_tool_metadata(&reconstructed)?;
            }
            prepared_events.push(reconstructed);
        }
    }
    let prepared = context
        .output_dir
        .join("native/prepared-replay-events.json");
    atomic_write_json(&prepared, &prepared_events)?;
    journal.append(
        "session_rebuilt",
        [(
            "prepared_only".into(),
            json!(context.request.mode == ReplayMode::PrepareOnly),
        )],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(prepared));
    }
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("OpenHands replay has no launch spec"))?;
    let replayed_trajectory = match context.request.mode {
        ReplayMode::ReplayOnly => context
            .output_dir
            .join("native/reconstructed-trajectory.json"),
        ReplayMode::ReplayAndContinue => {
            context.output_dir.join("native/continued-trajectory.json")
        }
        ReplayMode::PrepareOnly => unreachable!("prepare-only returned before OpenHands launch"),
    };
    let mut command = agent_command(&launch.entrypoint, context);
    command.args(["-m", "openhands.core.main"]);
    command.env("REPLAY_TRAJECTORY_PATH", &prepared);
    command.env("SAVE_TRAJECTORY_PATH", &replayed_trajectory);
    command.env("FILE_STORE", "local");
    command.env(
        "FILE_STORE_PATH",
        context.state_dir.join("openhands-file-store"),
    );
    command.env("RUNTIME", "local");
    command.env("SU_TO_USER", "false");
    command.env("RUN_AS_OPENHANDS", "false");
    command.env("SKIP_DEPENDENCY_CHECK", "1");
    command.env("INIT_PLUGIN_TIMEOUT", "240");
    command.env("AGENT_ENABLE_PROMPT_EXTENSIONS", "false");
    command.env("AGENT_ENABLE_BROWSING", "false");
    command.env("ENABLE_BROWSER", "false");
    command.env("SANDBOX_ENABLE_AUTO_LINT", "true");
    command.env(
        "SANDBOX_VOLUMES",
        format!("{}:/workspace:rw", context.request.workspace.display()),
    );
    prepend_openhands_runtime_tools(&mut command, launch)?;
    command.env(
        "OPENAI_CUSTOM_HEADERS",
        format!("X-LiteLLM-Session-ID: {}", context.session_id),
    );
    let iteration_limit = match context.request.mode {
        ReplayMode::ReplayOnly => Some(plan.prefix_model_turns),
        ReplayMode::ReplayAndContinue => context.request.max_steps,
        ReplayMode::PrepareOnly => None,
    };
    if let Some(max) = iteration_limit {
        command.env("MAX_ITERATIONS", max.to_string());
    }
    journal.append("continuation_started", std::iter::empty())?;
    let log = context.output_dir.join("logs/openhands.log");
    fs::create_dir_all(log.parent().expect("OpenHands log has a parent"))
        .replay_context(ReplayErrorKind::Executor, "create OpenHands log directory")?;
    let output = run_process(ProcessSpec {
        command,
        stdin: Some(b"\n".to_vec()),
        timeout: Duration::from_secs(24 * 60 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log.clone(),
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
    if !output.stderr_tail.is_empty() {
        rendered.push('\n');
        rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
    }
    let fatal_marker = openhands_fatal_controller_marker(&rendered);
    if output.timed_out || !output.status.success() || !replayed_trajectory.is_file() {
        let detail = fatal_marker
            .map(|marker| format!("; OpenHands controller reported {marker:?}"))
            .unwrap_or_default();
        return Err(ReplayError::classify_continuation(
            format!(
                "OpenHands replay/continuation exited {}{detail}; see {}",
                output.status,
                log.display()
            ),
            &rendered,
        ));
    }
    if let Some(marker) = fatal_marker {
        return Err(ReplayError::classify_continuation(
            format!(
                "OpenHands controller reported {marker:?} despite exiting successfully; partial trajectory retained at {}; see {}",
                replayed_trajectory.display(),
                log.display()
            ),
            &rendered,
        ));
    }
    let continued_events: Vec<Value> =
        serde_json::from_slice(&read_regular_file(&replayed_trajectory)?).replay_context(
            ReplayErrorKind::Trajectory,
            "parse replayed OpenHands trajectory",
        )?;
    let complete = openhands_complete_batches(&continued_events)?;
    if complete.len() < plan.after_step {
        return Err(ReplayError::trajectory(
            "OpenHands output lost replayed action/observation batches",
        ));
    }
    if context.request.mode == ReplayMode::ReplayOnly && complete.len() != plan.after_step {
        return Err(ReplayError::continuation(format!(
            "OpenHands replay-only crossed the selected boundary: expected {} actions, observed {}",
            plan.after_step,
            complete.len()
        )));
    }
    if context
        .request
        .max_steps
        .is_some_and(|max_steps| complete.len() > max_steps)
    {
        return Err(ReplayError::continuation(format!(
            "OpenHands exceeded the total max_steps budget: allowed {}, observed {} actions",
            context.request.max_steps.unwrap(),
            complete.len()
        )));
    }
    let replayed = &complete[..plan.after_step];
    let observations = plan
        .calls()
        .zip(replayed.iter())
        .map(|(call, (_, observation))| FreshObservation {
            call_id: call.call_id.clone(),
            content: openhands_observation_content(observation),
            is_error: observation.get("observation").and_then(Value::as_str) == Some("error"),
            return_code: None,
            duration_ms: 0,
            truncated: false,
            metadata: BTreeMap::new(),
        })
        .collect::<Vec<_>>();
    let comparisons = plan
        .calls()
        .zip(&observations)
        .map(|(call, fresh)| {
            json!({
                "call_id": call.call_id,
                "tool": call.name,
                "exact": call.original_observation == fresh.content
                    && call.original_is_error == fresh.is_error,
                "original_is_error": call.original_is_error,
                "replayed_is_error": fresh.is_error,
            })
        })
        .collect::<Vec<_>>();
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )?;
    let continued_steps = complete.len() - plan.after_step;
    journal.append(
        "continuation_finished",
        [
            ("continued_steps".into(), json!(continued_steps)),
            ("agent_error".into(), Value::Null),
        ],
    )?;
    let reached_max_steps = context.request.mode == ReplayMode::ReplayAndContinue
        && context
            .request
            .max_steps
            .is_some_and(|max_steps| complete.len() == max_steps)
        && rendered.contains("Agent reached maximum iteration");
    Ok(ReplayOutcome {
        status: if context.request.mode == ReplayMode::ReplayOnly {
            "replayed".into()
        } else if reached_max_steps {
            "max_steps".into()
        } else {
            "completed".into()
        },
        reconstructed_path: (context.request.mode == ReplayMode::ReplayOnly)
            .then_some(replayed_trajectory.clone()),
        continued_path: (context.request.mode == ReplayMode::ReplayAndContinue)
            .then_some(replayed_trajectory),
        observations,
        continued_steps,
        metadata: json!({}),
    })
}

fn openhands_fatal_controller_marker(output: &str) -> Option<&'static str> {
    if output.contains("Agent reached maximum iteration") {
        return None;
    }
    [
        "AgentState.ERROR",
        "Error while running the agent",
        "There was an unexpected error while running the agent",
    ]
    .into_iter()
    .find(|marker| output.contains(marker))
}

fn openhands_observation_content(observation: &Value) -> Value {
    json!({
        "observation": observation.get("observation"),
        "message": observation.get("message"),
        "args": observation.get("args"),
    })
}

fn openhands_complete_batches(events: &[Value]) -> Result<Vec<(&Value, &Value)>, ReplayError> {
    let mut observations = BTreeMap::new();
    for event in events {
        let Some(cause) = event
            .get("observation")
            .filter(|value| !value.is_null())
            .and_then(|_| event.get("cause"))
            .and_then(Value::as_i64)
        else {
            continue;
        };
        if observations.insert(cause, event).is_some() {
            return Err(ReplayError::trajectory(format!(
                "multiple OpenHands observations for action {cause}"
            )));
        }
    }

    let supported = ["run", "read", "edit", "run_ipython", "think"];
    let mut batches = Vec::new();
    for event in events {
        let action = event.get("action").and_then(Value::as_str);
        if event.get("source").and_then(Value::as_str) != Some("agent")
            || matches!(action, None | Some("system" | "finish" | "message"))
        {
            continue;
        }
        let action = action.unwrap();
        if !supported.contains(&action) {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!("unsupported OpenHands action {action:?}"),
            ));
        }
        let id = event_id(event)?;
        let Some(observation) = observations.get(&id) else {
            break;
        };
        batches.push((event, *observation));
    }
    Ok(batches)
}

fn prepend_openhands_runtime_tools(
    command: &mut Command,
    launch: &LaunchSpec,
) -> Result<(), ReplayError> {
    let inferred_root = launch
        .entrypoint
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/"));
    let tools = launch
        .runtime_root
        .as_deref()
        .unwrap_or(inferred_root)
        .join("tools");
    if !tools.is_dir() {
        return Ok(());
    }
    let current = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
    let paths = std::iter::once(tools.clone()).chain(std::env::split_paths(&current));
    let path = std::env::join_paths(paths).map_err(|error| {
        ReplayError::configuration(format!(
            "cannot prepend OpenHands runtime tools {} to PATH: {error}",
            tools.display()
        ))
    })?;
    command.env("PATH", path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use serde_json::{json, Value};

    use super::{
        openhands_action_signature, openhands_complete_batches, openhands_fatal_controller_marker,
        openhands_observation_content, openhands_reconstructed_tool_metadata,
        prepend_openhands_runtime_tools, LaunchSpec,
    };

    #[test]
    fn openhands_reconstructs_legacy_native_tool_metadata() {
        let event = json!({
            "id": 7,
            "source": "agent",
            "action": "read",
            "args": {"path": "/workspace/file", "view_range": [1, 2], "thought": "inspect"},
        });
        let metadata = openhands_reconstructed_tool_metadata(&event).unwrap();
        assert_eq!(metadata["function_name"], "str_replace_editor");
        assert_eq!(metadata["tool_call_id"], "sandbox-playback-replay-7");
        let arguments = metadata["model_response"]["choices"][0]["message"]["tool_calls"][0]
            ["function"]["arguments"]
            .as_str()
            .unwrap();
        let arguments: Value = serde_json::from_str(arguments).unwrap();
        assert_eq!(arguments["command"], "view");
        assert_eq!(arguments["path"], "/workspace/file");
    }

    #[test]
    fn openhands_signature_separates_visible_text_reasoning_and_tool_arguments() {
        let event = json!({
            "id": 7,
            "source": "agent",
            "action": "run",
            "args": {"command": "pwd", "thought": "legacy thought"},
            "tool_call_metadata": {
                "model_response": {
                    "choices": [{
                        "message": {
                            "content": "visible preamble",
                            "reasoning_content": "hidden reasoning"
                        }
                    }]
                }
            }
        });

        let signature = openhands_action_signature(&event);

        assert_eq!(signature["text"], "visible preamble");
        assert_eq!(signature["reasoning"], "hidden reasoning");
        assert_eq!(
            signature["tools"][0]["arguments"],
            json!({"command": "pwd"})
        );
    }

    #[test]
    fn openhands_complete_batches_preserve_fresh_observations() {
        let events = vec![
            json!({
                "id": 5,
                "source": "agent",
                "action": "run",
                "args": {"command": "pwd"},
            }),
            json!({
                "id": 6,
                "source": "environment",
                "observation": "run",
                "cause": 5,
                "message": "ok",
                "args": {"command": "pwd", "metadata": {"exit_code": 0}},
            }),
        ];
        let batches = openhands_complete_batches(&events).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].0["id"], 5);
        assert_eq!(
            openhands_observation_content(batches[0].1),
            json!({
                "observation": "run",
                "message": "ok",
                "args": {"command": "pwd", "metadata": {"exit_code": 0}},
            })
        );
    }

    #[test]
    fn openhands_runtime_tools_are_prepended_to_path() {
        let runtime = tempfile::tempdir().unwrap();
        let bin = runtime.path().join("bin/openhands-python");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::create_dir(runtime.path().join("tools")).unwrap();
        let launch = LaunchSpec {
            entrypoint: bin,
            version: "0.53.0".into(),
            source: "explicit_entrypoint".into(),
            runtime_root: None,
        };
        let mut command = Command::new(&launch.entrypoint);
        prepend_openhands_runtime_tools(&mut command, &launch).unwrap();
        let path = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "PATH").then(|| value.expect("PATH value").to_os_string())
            })
            .expect("PATH override");
        let first = std::env::split_paths(&path).next().unwrap();
        assert_eq!(first, runtime.path().join("tools"));
    }

    #[test]
    fn openhands_zero_exit_controller_errors_are_detected_for_partial_results() {
        assert_eq!(
            openhands_fatal_controller_marker("Error while running the agent"),
            Some("Error while running the agent")
        );
        assert_eq!(
            openhands_fatal_controller_marker("Agent reached maximum iteration AgentState.ERROR"),
            None
        );
    }
}
