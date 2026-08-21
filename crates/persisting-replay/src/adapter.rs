use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::claude_bridge::ClaudeBridgeHandle;
use crate::claude_resume::ResumeTransportManifest;
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write, atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AgentKind, FreshObservation, PlaybackRequest, ReplayOutcome, ReplayPlan, ToolBatch, ToolCall,
};

const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const SUPPORTED_CLAUDE_TOOLS: &[&str] = &[
    "Agent",
    "Bash",
    "Edit",
    "Find",
    "Glob",
    "Grep",
    "MultiEdit",
    "Read",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "TaskUpdate",
    "TodoWrite",
    "Write",
];

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub entrypoint: PathBuf,
    pub version: String,
    pub source: String,
    pub runtime_root: Option<PathBuf>,
}

pub struct RunContext<'a> {
    pub request: &'a PlaybackRequest,
    pub state_dir: &'a Path,
    pub output_dir: &'a Path,
    pub launch: Option<&'a LaunchSpec>,
    pub session_id: &'a str,
    pub nonce: &'a str,
}

pub fn resolve_launch_spec(request: &PlaybackRequest) -> Result<Option<LaunchSpec>, ReplayError> {
    if request.agent_entrypoint.is_some() && request.agent_runtime.is_some() {
        return Err(ReplayError::configuration(
            "agent entrypoint and agent runtime are mutually exclusive",
        ));
    }
    if request.replay_only && request.agent_entrypoint.is_none() && request.agent_runtime.is_none()
    {
        return Ok(None);
    }
    let (entrypoint, source, runtime_root, declared_version) =
        if let Some(runtime_root) = &request.agent_runtime {
            let root = canonicalize(
                runtime_root,
                ReplayErrorKind::Configuration,
                "agent runtime",
            )?;
            let manifest_path = root.join("sandbox-playback-agent.json");
            let manifest: RuntimeManifest =
                serde_json::from_slice(&read_regular_file(&manifest_path)?).replay_context(
                    ReplayErrorKind::Configuration,
                    format!("parse agent runtime manifest {}", manifest_path.display()),
                )?;
            if manifest.schema_version != "sandbox-playback.agent-runtime/v1" {
                return Err(ReplayError::configuration(
                    "agent runtime schema_version must be sandbox-playback.agent-runtime/v1",
                ));
            }
            if manifest.agent != request.agent.as_str() {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedAgent,
                    format!(
                        "agent runtime declares {:?}, requested {:?}",
                        manifest.agent,
                        request.agent.as_str()
                    ),
                ));
            }
            if manifest.version != request.agent.supported_version() {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedVersion,
                    format!(
                        "agent runtime declares {:?}; profile requires {}",
                        manifest.version,
                        request.agent.supported_version()
                    ),
                ));
            }
            let relative = safe_relative(&manifest.entrypoint)?;
            (
                root.join(relative),
                "runtime_manifest".to_owned(),
                Some(root),
                Some(manifest.version),
            )
        } else {
            let entrypoint = request.agent_entrypoint.clone().ok_or_else(|| {
                ReplayError::configuration(
                    "non-replay-only mode requires --agent-entrypoint or --agent-runtime",
                )
            })?;
            (entrypoint, "explicit_entrypoint".to_owned(), None, None)
        };
    if !entrypoint.is_absolute() {
        return Err(ReplayError::configuration(
            "agent entrypoint must be an absolute path",
        ));
    }
    let entrypoint = canonicalize(
        &entrypoint,
        ReplayErrorKind::Configuration,
        "agent entrypoint",
    )?;
    if !entrypoint.is_file() {
        return Err(ReplayError::configuration(format!(
            "agent entrypoint is not a regular file: {}",
            entrypoint.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if entrypoint
            .metadata()
            .map(|m| m.permissions().mode() & 0o111 == 0)
            .unwrap_or(true)
        {
            return Err(ReplayError::configuration(format!(
                "agent entrypoint is not executable: {}",
                entrypoint.display()
            )));
        }
    }
    let version = probe_version(request.agent, &entrypoint)?;
    if declared_version
        .as_deref()
        .is_some_and(|declared| declared != version)
    {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            "agent runtime manifest and executable versions differ",
        ));
    }
    Ok(Some(LaunchSpec {
        entrypoint,
        version,
        source,
        runtime_root,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeManifest {
    schema_version: String,
    agent: String,
    version: String,
    entrypoint: PathBuf,
    #[serde(default, rename = "paths")]
    _paths: BTreeMap<String, PathBuf>,
}

fn safe_relative(path: &Path) -> Result<PathBuf, ReplayError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ReplayError::configuration(
            "agent runtime entrypoint must be a non-empty relative path without '..'",
        ));
    }
    Ok(path.to_path_buf())
}

fn probe_version(agent: AgentKind, entrypoint: &Path) -> Result<String, ReplayError> {
    let expected = agent.supported_version();
    let mut command = Command::new(entrypoint);
    match agent {
        AgentKind::ClaudeCode | AgentKind::MiniSweAgent => {
            command.arg("--version");
        }
        AgentKind::Openhands => {
            command.args([
                "-c",
                "import importlib.metadata;print(importlib.metadata.version('openhands-ai'))",
            ]);
        }
        AgentKind::SweAgent => {
            command.args([
                "-c",
                "import importlib.metadata;print(importlib.metadata.version('sweagent'))",
            ]);
        }
    }
    command.env_remove("PYTHONHOME");
    command.env_remove("PYTHONPATH");
    command.env_remove("VIRTUAL_ENV");
    if agent == AgentKind::MiniSweAgent {
        let runtime = mini_python_runtime(entrypoint)?;
        configure_mini_python_environment(&mut command, &runtime)?;
    }
    let output = command.output().replay_context(
        ReplayErrorKind::UnsupportedVersion,
        format!(
            "probe {} version from {}",
            agent.as_str(),
            entrypoint.display()
        ),
    )?;
    let rendered = String::from_utf8_lossy(if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    });
    let detected = probed_version(agent, &rendered, expected);
    // mini-swe-agent 2.4.6 prints its version before loading the global config.
    // In a freshly provisioned sandbox that later config load can exit non-zero,
    // but the unambiguous version banner is still a valid executable probe.
    let status_is_acceptable =
        output.status.success() || (agent == AgentKind::MiniSweAgent && detected == Some(expected));
    if !status_is_acceptable || detected != Some(expected) {
        return Err(ReplayError::new(
            ReplayErrorKind::UnsupportedVersion,
            format!(
                "{} profile requires {}, got {:?} from {}",
                agent.as_str(),
                expected,
                rendered.trim(),
                entrypoint.display()
            ),
        ));
    }
    Ok(expected.to_owned())
}

fn probed_version<'a>(agent: AgentKind, rendered: &'a str, expected: &'a str) -> Option<&'a str> {
    if agent != AgentKind::MiniSweAgent {
        return rendered.contains(expected).then_some(expected);
    }

    const PREFIX: &str = "This is mini-swe-agent version ";
    rendered.lines().find_map(|line| {
        let version = line
            .trim()
            .strip_prefix(PREFIX)?
            .split_whitespace()
            .next()?
            .trim_end_matches('.');
        (version == expected).then_some(version)
    })
}

pub fn build_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    match request.agent {
        AgentKind::ClaudeCode => build_claude_plan(request),
        AgentKind::MiniSweAgent => build_mini_plan(request),
        AgentKind::Openhands => build_openhands_plan(request),
        AgentKind::SweAgent => build_swe_plan(request),
    }
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

pub fn run(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    match plan.agent {
        AgentKind::ClaudeCode => run_claude(plan, context, journal),
        AgentKind::MiniSweAgent => run_mini(plan, context, journal),
        AgentKind::Openhands => run_openhands(plan, context, journal),
        AgentKind::SweAgent => run_swe(plan, context, journal),
    }
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
            if !SUPPORTED_CLAUDE_TOOLS.contains(&call.name.as_str())
                || (call.name == "Bash"
                    && call
                        .arguments
                        .get("run_in_background")
                        .and_then(Value::as_bool)
                        == Some(true))
            {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedVersion,
                    format!(
                        "unsupported Claude replay tool call {}({}) inside the selected prefix",
                        call.name, call.call_id
                    ),
                ));
            }
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

fn build_swe_plan(request: &PlaybackRequest) -> Result<ReplayPlan, ReplayError> {
    let raw = read_regular_file(&request.trajectory)?;
    let mut value: Value = serde_json::from_slice(&raw).replay_context(
        ReplayErrorKind::Trajectory,
        "invalid SWE-agent trajectory JSON",
    )?;
    for field in ["trajectory", "history", "replay_config"] {
        if value.get(field).is_none() {
            return Err(ReplayError::trajectory(format!(
                "SWE-agent trajectory is missing {field}"
            )));
        }
    }
    resolve_swe_problem_asset(&mut value, request.trajectory_assets.as_deref())?;
    let trajectory = value["trajectory"]
        .as_array()
        .ok_or_else(|| ReplayError::trajectory("SWE-agent trajectory must be an array"))?;
    let history: Vec<_> = value["history"]
        .as_array()
        .ok_or_else(|| ReplayError::trajectory("SWE-agent history must be an array"))?
        .iter()
        .filter(|item| item.get("role").and_then(Value::as_str) == Some("assistant"))
        .collect();
    check_boundary(request.after_step, trajectory.len().min(history.len()))?;
    let original_next_action = trajectory.get(request.after_step).map(|step| {
        json!({
            "text": "",
            "reasoning": step.get("thought").and_then(Value::as_str).unwrap_or_default(),
            "tools": [{
                "name": "swe_agent_action",
                "arguments": {"raw_action": step.get("action").cloned().unwrap_or(Value::Null)},
            }],
        })
    });
    let mut batches = Vec::new();
    for index in 0..request.after_step {
        let step = &trajectory[index];
        let assistant = history[index];
        let action = step
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ReplayError::trajectory("SWE-agent step has no action"))?;
        if action.trim() == "submit" || action.trim_start().starts_with("submit\n") {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                "SWE-agent submit cannot appear inside a replay prefix",
            ));
        }
        let observation = step
            .get("observation")
            .and_then(Value::as_str)
            .ok_or_else(|| ReplayError::trajectory("SWE-agent step has no observation"))?;
        let calls = assistant
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let call_id = if calls.len() == 1 {
            calls[0]
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("swe-agent-step-{}", index + 1))
        } else {
            format!("swe-agent-step-{}", index + 1)
        };
        batches.push(ToolBatch {
            ordinal: index + 1,
            native_locator: format!("trajectory:{index}"),
            tool_calls: vec![ToolCall {
                ordinal: index + 1,
                call_id,
                name: "swe_agent_action".into(),
                arguments: json!({"raw_action": action}),
                original_observation: Value::String(observation.to_owned()),
                original_is_error: false,
                native: json!({"assistant": assistant}),
            }],
            assistant_text: step
                .get("thought")
                .and_then(Value::as_str)
                .or_else(|| assistant.get("content").and_then(Value::as_str))
                .unwrap_or_default()
                .to_owned(),
            native: json!({"state": step.get("state")}),
        });
    }
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
        native: value,
        original_next_action,
    })
}

fn resolve_swe_problem_asset(value: &mut Value, assets: Option<&Path>) -> Result<(), ReplayError> {
    let replay_config = value
        .get_mut("replay_config")
        .ok_or_else(|| ReplayError::trajectory("SWE-agent replay_config is required"))?;
    if replay_config.is_string() {
        let encoded = replay_config.as_str().unwrap();
        *replay_config = serde_json::from_str(encoded).replay_context(
            ReplayErrorKind::Trajectory,
            "invalid encoded SWE-agent replay_config",
        )?;
    }
    let Some(problem) = replay_config.get_mut("problem_statement") else {
        return Ok(());
    };
    if !matches!(
        problem.get("type").and_then(Value::as_str),
        Some("file" | "path")
    ) {
        return Ok(());
    }
    let root = assets.ok_or_else(|| {
        ReplayError::trajectory("SWE-agent file problem_statement requires trajectory_assets")
    })?;
    let relative = problem
        .get("path")
        .or_else(|| problem.get("file"))
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::trajectory("SWE-agent problem asset path is invalid"))?;
    let relative = safe_relative(Path::new(relative))?;
    let root = canonicalize(root, ReplayErrorKind::Trajectory, "trajectory assets")?;
    let path = canonicalize(
        &root.join(relative),
        ReplayErrorKind::Trajectory,
        "trajectory asset",
    )?;
    if !path.starts_with(&root) {
        return Err(ReplayError::trajectory(
            "SWE-agent trajectory asset escapes its root",
        ));
    }
    let text = String::from_utf8(read_regular_file(&path)?).replay_context(
        ReplayErrorKind::Trajectory,
        "SWE-agent problem asset is not UTF-8",
    )?;
    let id = problem
        .get("id")
        .cloned()
        .unwrap_or_else(|| json!("replay"));
    *problem = json!({"type": "text", "text": text, "id": id});
    Ok(())
}

fn check_boundary(after_step: usize, complete: usize) -> Result<(), ReplayError> {
    if after_step == 0 || after_step > complete {
        return Err(ReplayError::trajectory(format!(
            "requested after-step {after_step}, trajectory has {complete} complete batches"
        )));
    }
    Ok(())
}

fn required_str<'a>(value: &'a Value, field: &str, context: &str) -> Result<&'a str, ReplayError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ReplayError::trajectory(format!("{context} has no {field}")))
}

fn run_claude(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let session_id = plan.native["session_id"]
        .as_str()
        .ok_or_else(|| ReplayError::trajectory("Claude plan lost its session ID"))?;
    let mut replacements = BTreeMap::new();
    let mut observations = Vec::new();
    let mut comparisons = Vec::new();
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
            let fresh = execute_claude_tool(call, &context.request.workspace)?;
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
    if context.request.replay_only {
        return Ok(ReplayOutcome {
            status: "replayed".into(),
            reconstructed_path: Some(reconstructed),
            continued_path: None,
            observations,
            continued_steps: 0,
            metadata: json!({"native_session_id": session_id}),
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
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().replay_context(
        ReplayErrorKind::Continuation,
        format!("start Claude Code {}", launch.entrypoint.display()),
    )?;
    let nonce_write = match child.stdin.take() {
        Some(mut stdin) => stdin
            .write_all(context.nonce.as_bytes())
            .replay_context(ReplayErrorKind::Continuation, "write Claude resume nonce"),
        None => Err(ReplayError::continuation("Claude stdin unavailable")),
    };
    if let Err(error) = nonce_write {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let output = child
        .wait_with_output()
        .replay_context(ReplayErrorKind::Continuation, "wait for Claude Code")?;
    let log = context.output_dir.join("logs/claude-code.jsonl");
    write_process_log(&log, &output)?;
    let process_error = if !output.status.success()
        && !expected_claude_max_turn_exit(&output.stdout, remaining_turns)
    {
        let rendered = render_output(&output);
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
        ],
    )?;
    Ok(ReplayOutcome {
        status: "completed".into(),
        reconstructed_path: Some(reconstructed),
        continued_path: Some(continued),
        observations,
        continued_steps,
        metadata: json!({
            "native_session_id": session_id,
            "validated_model_requests": validated_model_requests,
            "model_transport": "sandbox-replay-claude-bridge",
        }),
    })
}

fn execute_claude_tool(call: &ToolCall, workspace: &Path) -> Result<FreshObservation, ReplayError> {
    let started = Instant::now();
    if call.original_is_error && claude_arguments_are_invalid(call) {
        return replay_original_observation(call, started, "input_validation_error");
    }
    let (content, is_error, return_code) = match call.name.as_str() {
        "Agent" => {
            if call.arguments.get("subagent_type").and_then(Value::as_str) != Some("Explore") {
                return Err(ReplayError::new(
                    ReplayErrorKind::UnsupportedVersion,
                    "Claude Agent replay supports only the read-only Explore subagent",
                ));
            }
            return replay_original_observation(call, started, "read_only_explore_agent");
        }
        "TaskOutput" => {
            // TaskOutput only retrieves the result of an already-launched subagent. It does not
            // execute a command or mutate the workspace, so preserve the source observation just
            // as we do for the read-only Explore Agent call that produced it.
            return replay_original_observation(call, started, "read_only_task_output");
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
            run_bash(command, workspace, timeout)?
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
        "Find" => (
            "Find is unavailable in the native Claude Code 2.1.220 replay profile; use Glob".into(),
            true,
            Some(1),
        ),
        "TaskCreate" | "TaskGet" | "TaskList" | "TaskUpdate" | "TodoWrite" => (
            json!({"replayed": true, "tool": call.name, "input": call.arguments}).to_string(),
            false,
            Some(0),
        ),
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
    reason: &str,
) -> Result<FreshObservation, ReplayError> {
    let mut metadata = BTreeMap::new();
    metadata.insert("opaque_source_observation".into(), json!(reason));
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
    let bytes = content.into_bytes();
    let truncated = bytes.len() > MAX_TOOL_OUTPUT_BYTES;
    let content = if truncated {
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
) -> Result<(String, bool, Option<i32>), ReplayError> {
    let mut process = Command::new("/bin/bash");
    process
        .args(["-c", command])
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    process.process_group(0);
    sanitized_environment(&mut process, true);
    let mut child = process.spawn().replay_context(
        ReplayErrorKind::Executor,
        "execute historical Claude Bash call",
    )?;
    let mut stdout = child.stdout.take().expect("stdout configured as piped");
    let mut stderr = child.stderr.take().expect("stderr configured as piped");
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().replay_context(
            ReplayErrorKind::Executor,
            "wait for historical Claude Bash call",
        )? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            #[cfg(not(unix))]
            let _ = child.kill();
            let status = child.wait().replay_context(
                ReplayErrorKind::Executor,
                "reap timed-out historical Claude Bash call",
            )?;
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| ReplayError::new(ReplayErrorKind::Internal, "Bash stdout reader panicked"))?
        .replay_context(ReplayErrorKind::Executor, "read historical Bash stdout")?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ReplayError::new(ReplayErrorKind::Internal, "Bash stderr reader panicked"))?
        .replay_context(ReplayErrorKind::Executor, "read historical Bash stderr")?;
    let mut content = String::from_utf8_lossy(&stdout).into_owned();
    if !stderr.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&String::from_utf8_lossy(&stderr));
    }
    if timed_out {
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
        timed_out || !status.success(),
        if timed_out { Some(124) } else { status.code() },
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
        [("prepared_only".into(), json!(context.request.replay_only))],
    )?;
    if context.request.replay_only {
        return Ok(prepared_outcome(path));
    }
    run_sdk_bridge(plan, context, journal, AgentKind::MiniSweAgent, &path)
}

fn run_swe(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    let mut prepared = plan.native.clone();
    prepared["trajectory"] =
        Value::Array(plan.native["trajectory"].as_array().unwrap()[..plan.after_step].to_vec());
    let mut assistant = 0;
    let mut history = Vec::new();
    for item in plan.native["history"].as_array().unwrap() {
        history.push(item.clone());
        if item.get("role").and_then(Value::as_str) == Some("assistant") {
            assistant += 1;
            if assistant == plan.after_step {
                break;
            }
        }
    }
    prepared["history"] = Value::Array(history);
    let path = context.output_dir.join("native/prepared-prefix.traj");
    atomic_write_json(&path, &prepared)?;
    journal.append(
        "session_rebuilt",
        [("prepared_only".into(), json!(context.request.replay_only))],
    )?;
    if context.request.replay_only {
        return Ok(prepared_outcome(path));
    }
    run_sdk_bridge(plan, context, journal, AgentKind::SweAgent, &path)
}

fn prepared_outcome(path: PathBuf) -> ReplayOutcome {
    ReplayOutcome {
        status: "prepared".into(),
        reconstructed_path: Some(path),
        continued_path: None,
        observations: Vec::new(),
        continued_steps: 0,
        metadata: json!({"replay_only_execution": false}),
    }
}

fn run_sdk_bridge(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    agent: AgentKind,
    prepared: &Path,
) -> Result<ReplayOutcome, ReplayError> {
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("SDK continuation has no launch spec"))?;
    if context
        .request
        .max_steps
        .is_some_and(|max| max <= plan.prefix_model_turns)
    {
        return Err(ReplayError::continuation(
            "max-steps is exhausted by the replay prefix",
        ));
    }
    let native_dir = context.output_dir.join("native");
    let logs_dir = context.output_dir.join("logs");
    fs::create_dir_all(&native_dir)
        .replay_context(ReplayErrorKind::Executor, "create native output directory")?;
    fs::create_dir_all(&logs_dir)
        .replay_context(ReplayErrorKind::Executor, "create Agent log directory")?;

    let (program, bridge_source, bridge_name, request_value, continued, observations_path) =
        match agent {
            AgentKind::MiniSweAgent => {
                let source = context.state_dir.join("mini-source.json");
                let continued = native_dir.join("continued-trajectory.json");
                let observations = context.state_dir.join("mini-fresh-observations.json");
                atomic_write_json(&source, &plan.native)?;
                let runtime = mini_python_runtime(&launch.entrypoint)?;
                let program = runtime
                    .loader
                    .clone()
                    .unwrap_or_else(|| runtime.python.clone());
                (
                    program,
                    include_str!("../assets/mini_swe_agent_runner.py"),
                    "mini-swe-agent-runner.py",
                    json!({
                        "source": source,
                        "continued": continued,
                        "observations": observations,
                        "workspace": context.request.workspace,
                        "after_step": plan.after_step,
                        "max_steps": context.request.max_steps,
                        "session_id": context.session_id,
                    }),
                    continued,
                    Some(observations),
                )
            }
            AgentKind::SweAgent => {
                let source = native_dir.join("continuation-source.traj");
                let run_output = native_dir.join("swe-agent-run");
                let continued = native_dir.join("continued-trajectory.traj");
                atomic_write_json(&source, &plan.native)?;
                (
                    launch.entrypoint.clone(),
                    include_str!("../assets/swe_agent_runner.py"),
                    "swe-agent-runner.py",
                    json!({
                        "trajectory": source,
                        "trajectory_assets": context.request.trajectory_assets,
                        "after_step": plan.after_step,
                        "workspace": context.request.workspace,
                        "output_dir": run_output,
                    }),
                    continued,
                    None,
                )
            }
            _ => {
                return Err(ReplayError::new(
                    ReplayErrorKind::Internal,
                    "SDK bridge selected for a non-SDK agent",
                ));
            }
        };

    let bridge = context.state_dir.join(bridge_name);
    let request_path = context
        .state_dir
        .join(format!("{}-request.json", agent.as_str()));
    atomic_write(&bridge, bridge_source.as_bytes())?;
    atomic_write_json(&request_path, &request_value)?;
    let mut command = agent_command(&program, context);
    if agent == AgentKind::MiniSweAgent {
        let runtime = mini_python_runtime(&launch.entrypoint)?;
        if runtime.loader.is_some() {
            let library_path = mini_python_library_path(&runtime)?.ok_or_else(|| {
                ReplayError::continuation("bundled mini-swe-agent Python has no library path")
            })?;
            let argv0 = runtime
                .virtual_env
                .as_deref()
                .map(|venv| venv.join("bin/python"))
                .unwrap_or_else(|| runtime.python.clone());
            command
                .arg("--argv0")
                .arg(argv0)
                .arg("--library-path")
                .arg(library_path)
                .arg(&runtime.python);
        }
        configure_mini_python_environment(&mut command, &runtime)?;
        command.env("MSWEA_CONFIGURED", "true");
        command.env("MSWEA_COST_TRACKING", "ignore_errors");
        command.env("SWE_EVAL_MINI_RUNTIME", "1");
    }
    command.arg(&bridge).arg(&request_path);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    journal.append("continuation_started", std::iter::empty())?;
    let output = command.output().replay_context(
        ReplayErrorKind::Continuation,
        format!("start {} replay bridge", agent.as_str()),
    )?;
    let log = logs_dir.join(format!("{}.log", agent.as_str()));
    write_process_log(&log, &output)?;
    if !output.status.success() {
        let rendered = render_output(&output);
        return Err(ReplayError::classify_continuation(
            format!(
                "{} replay/continuation exited {}; see {}",
                agent.as_str(),
                output.status,
                log.display()
            ),
            &rendered,
        ));
    }

    let (observations, continued_steps) = if agent == AgentKind::MiniSweAgent {
        if !continued.is_file() {
            return Err(ReplayError::continuation(format!(
                "mini-swe-agent produced no continued trajectory; see {}",
                log.display()
            )));
        }
        let raw_observations: Vec<Value> = serde_json::from_slice(&read_regular_file(
            observations_path.as_ref().expect("mini observations path"),
        )?)
        .replay_context(
            ReplayErrorKind::Trajectory,
            "parse mini-swe-agent fresh observations",
        )?;
        if raw_observations.len() != plan.calls().count() {
            return Err(ReplayError::trajectory(
                "mini-swe-agent output lost replayed observations",
            ));
        }
        let observations = plan
            .calls()
            .zip(raw_observations)
            .map(|(call, value)| FreshObservation {
                call_id: call.call_id.clone(),
                content: value.get("content").cloned().unwrap_or(Value::Null),
                is_error: value
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                return_code: value
                    .get("return_code")
                    .and_then(Value::as_i64)
                    .map(|code| code as i32),
                duration_ms: value
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as u128,
                truncated: false,
                metadata: BTreeMap::new(),
            })
            .collect::<Vec<_>>();
        let continued_value: Value = serde_json::from_slice(&read_regular_file(&continued)?)
            .replay_context(
                ReplayErrorKind::Trajectory,
                "parse continued mini-swe-agent trajectory",
            )?;
        let action_count = continued_value["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| {
                        message
                            .get("extra")
                            .and_then(|extra| extra.get("actions"))
                            .and_then(Value::as_array)
                            .is_some_and(|actions| !actions.is_empty())
                    })
                    .count()
            })
            .unwrap_or_default();
        (observations, action_count.saturating_sub(plan.after_step))
    } else {
        let run_output = request_value["output_dir"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| {
                ReplayError::new(ReplayErrorKind::Internal, "SWE-agent output missing")
            })?;
        let mut candidates = Vec::new();
        collect_extension(&run_output, "traj", &mut candidates)?;
        if candidates.len() != 1 {
            return Err(ReplayError::continuation(format!(
                "SWE-agent continuation produced {} trajectory files",
                candidates.len()
            )));
        }
        atomic_write(&continued, &read_regular_file(&candidates[0])?)?;
        let replayed: Value = serde_json::from_slice(&read_regular_file(&continued)?)
            .replay_context(
                ReplayErrorKind::Trajectory,
                "parse continued SWE-agent trajectory",
            )?;
        let steps = replayed["trajectory"]
            .as_array()
            .ok_or_else(|| ReplayError::trajectory("continued SWE-agent trajectory is invalid"))?;
        if steps.len() < plan.after_step {
            return Err(ReplayError::trajectory(
                "SWE-agent output lost replayed steps",
            ));
        }
        let observations = plan
            .calls()
            .zip(steps.iter())
            .map(|(call, step)| FreshObservation {
                call_id: call.call_id.clone(),
                content: step.get("observation").cloned().unwrap_or(Value::Null),
                is_error: false,
                return_code: None,
                duration_ms: 0,
                truncated: false,
                metadata: BTreeMap::new(),
            })
            .collect::<Vec<_>>();
        let continued_steps = steps[plan.after_step..]
            .iter()
            .filter(|step| {
                step.get("action")
                    .and_then(Value::as_str)
                    .is_some_and(|action| !action.trim().is_empty())
            })
            .count();
        (observations, continued_steps)
    };
    if continued_steps == 0 {
        return Err(ReplayError::continuation(format!(
            "{} produced no actionable continuation step; see {}",
            agent.as_str(),
            log.display()
        )));
    }
    let comparisons: Vec<_> = plan
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
        .collect();
    atomic_write_json(
        &context.output_dir.join("observation-comparison.json"),
        &comparisons,
    )?;
    journal.append(
        "continuation_finished",
        [
            ("return_code".into(), json!(output.status.code())),
            ("continued_steps".into(), json!(continued_steps)),
        ],
    )?;
    Ok(ReplayOutcome {
        status: "completed".into(),
        reconstructed_path: Some(prepared.to_path_buf()),
        continued_path: Some(continued),
        observations,
        continued_steps,
        metadata: json!({"sdk_bridge": bridge_name}),
    })
}

#[derive(Debug)]
struct MiniPythonRuntime {
    python: PathBuf,
    loader: Option<PathBuf>,
    python_home: Option<PathBuf>,
    virtual_env: Option<PathBuf>,
    library_paths: Vec<PathBuf>,
}

fn mini_python_runtime(entrypoint: &Path) -> Result<MiniPythonRuntime, ReplayError> {
    if let Some(local_root) = entrypoint.parent().and_then(Path::parent) {
        let uv_root = local_root.join("share/uv");
        let virtual_env = uv_root.join("tools/mini-swe-agent");
        let python = virtual_env.join("bin/python");
        if python.is_file() {
            let python = fs::canonicalize(&python).replay_context(
                ReplayErrorKind::Continuation,
                format!(
                    "resolve bundled mini-swe-agent Python from {}",
                    python.display()
                ),
            )?;
            let python_home = python
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| ReplayError::continuation("bundled Python has no prefix"))?
                .to_path_buf();
            if !python_home.join("lib/python3.12/encodings").is_dir() {
                return Err(ReplayError::continuation(format!(
                    "bundled mini-swe-agent Python has no standard library below {}",
                    python_home.display()
                )));
            }
            let loader = uv_root.join("sweeval-system-libs/ld-linux-x86-64.so.2");
            if !loader.is_file() {
                return Err(ReplayError::continuation(format!(
                    "bundled mini-swe-agent Python loader does not exist: {}",
                    loader.display()
                )));
            }
            return Ok(MiniPythonRuntime {
                python,
                loader: Some(loader),
                python_home: Some(python_home.clone()),
                virtual_env: Some(virtual_env),
                library_paths: vec![uv_root.join("sweeval-system-libs"), python_home.join("lib")],
            });
        }
    }

    let prefix = read_regular_file(entrypoint)?;
    if let Some(first) = prefix.split(|byte| *byte == b'\n').next() {
        if let Some(shebang) = first.strip_prefix(b"#!") {
            let rendered = String::from_utf8_lossy(shebang);
            let words: Vec<_> = rendered.split_whitespace().collect();
            if words.first() == Some(&"/usr/bin/env") {
                if let Some(program) = words.get(1) {
                    if program.contains("python") {
                        return Ok(MiniPythonRuntime {
                            python: PathBuf::from(program),
                            loader: None,
                            python_home: None,
                            virtual_env: None,
                            library_paths: Vec::new(),
                        });
                    }
                }
            } else if let Some(program) = words.first() {
                if program.contains("python") {
                    return Ok(MiniPythonRuntime {
                        python: PathBuf::from(program),
                        loader: None,
                        python_home: None,
                        virtual_env: None,
                        library_paths: Vec::new(),
                    });
                }
            }
        }
    }
    for name in ["python3", "python"] {
        let candidate = entrypoint.parent().unwrap_or(Path::new("/")).join(name);
        if candidate.is_file() {
            return Ok(MiniPythonRuntime {
                python: candidate,
                loader: None,
                python_home: None,
                virtual_env: None,
                library_paths: Vec::new(),
            });
        }
    }
    Err(ReplayError::continuation(
        "mini-swe-agent entrypoint does not expose its Python interpreter",
    ))
}

fn mini_python_library_path(runtime: &MiniPythonRuntime) -> Result<Option<OsString>, ReplayError> {
    let paths = runtime
        .library_paths
        .iter()
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return Ok(None);
    }
    std::env::join_paths(paths).map(Some).map_err(|error| {
        ReplayError::configuration(format!(
            "cannot construct mini-swe-agent Python library path: {error}"
        ))
    })
}

fn configure_mini_python_environment(
    command: &mut Command,
    runtime: &MiniPythonRuntime,
) -> Result<(), ReplayError> {
    if let Some(python_home) = &runtime.python_home {
        command.env("PYTHONHOME", python_home);
    }
    if let Some(virtual_env) = &runtime.virtual_env {
        command.env("VIRTUAL_ENV", virtual_env);
        command.env(
            "PYTHONPATH",
            virtual_env.join("lib/python3.12/site-packages"),
        );
        let current = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        let paths = std::iter::once(virtual_env.join("bin")).chain(std::env::split_paths(&current));
        let path = std::env::join_paths(paths).map_err(|error| {
            ReplayError::configuration(format!(
                "cannot prepend mini-swe-agent virtual environment to PATH: {error}"
            ))
        })?;
        command.env("PATH", path);
    }
    if let Some(library_path) = mini_python_library_path(runtime)? {
        command.env("LD_LIBRARY_PATH", library_path);
    }
    Ok(())
}

fn collect_extension(
    root: &Path,
    extension: &str,
    output: &mut Vec<PathBuf>,
) -> Result<(), ReplayError> {
    if !root.exists() {
        return Ok(());
    }
    if root.is_file() {
        if root.extension().and_then(|value| value.to_str()) == Some(extension) {
            output.push(root.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(root).replay_context(
        ReplayErrorKind::Continuation,
        format!("scan Agent output {}", root.display()),
    )? {
        let path = entry
            .replay_context(ReplayErrorKind::Continuation, "read Agent output entry")?
            .path();
        collect_extension(&path, extension, output)?;
    }
    Ok(())
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
        [("prepared_only".into(), json!(context.request.replay_only))],
    )?;
    if context.request.replay_only {
        return Ok(prepared_outcome(prepared));
    }
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("OpenHands continuation has no launch spec"))?;
    if context
        .request
        .max_steps
        .is_some_and(|max| max <= plan.prefix_model_turns)
    {
        return Err(ReplayError::continuation(
            "max-steps is exhausted by the replay prefix",
        ));
    }
    let continued = context.output_dir.join("native/continued-trajectory.json");
    let mut command = agent_command(&launch.entrypoint, context);
    command.args(["-m", "openhands.core.main"]);
    command.env("REPLAY_TRAJECTORY_PATH", &prepared);
    command.env("SAVE_TRAJECTORY_PATH", &continued);
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
    if let Some(max) = context.request.max_steps {
        command.env("MAX_ITERATIONS", (max + 1).to_string());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    journal.append("continuation_started", std::iter::empty())?;
    let mut child = command.spawn().replay_context(
        ReplayErrorKind::Continuation,
        "start OpenHands replay/continuation",
    )?;
    child
        .stdin
        .take()
        .ok_or_else(|| ReplayError::continuation("OpenHands stdin unavailable"))?
        .write_all(b"\n")
        .replay_context(ReplayErrorKind::Continuation, "write OpenHands stdin")?;
    let output = child
        .wait_with_output()
        .replay_context(ReplayErrorKind::Continuation, "wait for OpenHands")?;
    let log = context.output_dir.join("logs/openhands.log");
    write_process_log(&log, &output)?;
    let rendered = render_output(&output);
    let fatal_marker = openhands_fatal_controller_marker(&rendered);
    if !output.status.success() || !continued.is_file() {
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
    let continued_events: Vec<Value> = serde_json::from_slice(&read_regular_file(&continued)?)
        .replay_context(
            ReplayErrorKind::Trajectory,
            "parse continued OpenHands trajectory",
        )?;
    let complete = openhands_complete_batches(&continued_events)?;
    if complete.len() < plan.after_step {
        return Err(ReplayError::trajectory(
            "OpenHands output lost replayed action/observation batches",
        ));
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
            ("agent_error".into(), json!(fatal_marker)),
        ],
    )?;
    Ok(ReplayOutcome {
        status: "completed".into(),
        reconstructed_path: Some(prepared),
        continued_path: Some(continued),
        observations,
        continued_steps,
        metadata: fatal_marker
            .map(|marker| {
                json!({
                    "agent_terminal_status": "error",
                    "agent_error": marker,
                })
            })
            .unwrap_or_else(|| json!({})),
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

fn agent_command(entrypoint: &Path, context: &RunContext<'_>) -> Command {
    let mut command = Command::new(entrypoint);
    command.current_dir(&context.request.workspace);
    sanitized_environment(&mut command, context.request.agent == AgentKind::ClaudeCode);
    if context.request.agent != AgentKind::ClaudeCode {
        command.env("X_LITELLM_SESSION_ID", context.session_id);
        command.env(
            "LITELLM_EXTRA_HEADERS",
            json!({"X-LiteLLM-Session-ID": context.session_id}).to_string(),
        );
    }
    command
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

fn sanitized_environment(command: &mut Command, strip_credentials: bool) {
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let rendered = name.to_string_lossy().to_ascii_uppercase();
        if !environment_name_allowed(&rendered, strip_credentials) {
            continue;
        }
        command.env(name, value);
    }
}

fn environment_name_allowed(rendered: &str, strip_credentials: bool) -> bool {
    let credential = ["API_KEY", "TOKEN", "SECRET", "AUTHORIZATION", "PASSWORD"]
        .iter()
        .any(|fragment| rendered.contains(fragment));
    let claude_provider_override = strip_credentials
        && matches!(
            rendered,
            "CLAUDE_CODE_USE_BEDROCK" | "CLAUDE_CODE_USE_VERTEX" | "CLAUDE_CODE_USE_FOUNDRY"
        );
    !(strip_credentials && credential)
        && !claude_provider_override
        && !matches!(rendered, "PYTHONHOME" | "PYTHONPATH" | "VIRTUAL_ENV")
}

fn write_process_log(path: &Path, output: &Output) -> Result<(), ReplayError> {
    let mut bytes = output.stdout.clone();
    if !output.stderr.is_empty() {
        if !bytes.ends_with(b"\n") {
            bytes.push(b'\n');
        }
        bytes.extend_from_slice(&output.stderr);
    }
    atomic_write(path, &bytes)
}

fn render_output(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
    use super::*;
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

        let observation = execute_claude_tool(&call, workspace.path()).unwrap();
        assert!(observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("input_validation_error"))
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

        let observation = execute_claude_tool(&call, workspace.path()).unwrap();
        assert!(!observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("read_only_explore_agent"))
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

        let observation = execute_claude_tool(&call, workspace.path()).unwrap();
        assert!(!observation.is_error);
        assert_eq!(observation.content, call.original_observation);
        assert_eq!(
            observation.metadata.get("opaque_source_observation"),
            Some(&json!("read_only_task_output"))
        );
    }

    #[test]
    fn claude_read_directory_and_missing_nested_path_are_tool_errors() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("directory")).unwrap();

        for file_path in ["directory", "missing/nested/file.txt"] {
            let observation = execute_claude_tool(
                &claude_tool_call("Read", json!({"file_path": file_path})),
                workspace.path(),
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
        let observation = execute_claude_tool(
            &claude_tool_call("Grep", json!({"search": "needle", "files": source})),
            workspace.path(),
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
        let observation = execute_claude_tool(
            &claude_tool_call("Grep", json!({"pattern": ""})),
            workspace.path(),
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
            replay_only: true,
            run_id: Some("test".into()),
            disable_thinking: false,
        };
        if !request.trajectory.exists() {
            return;
        }
        let plan = build_plan(&request).unwrap();
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
            replay_only: true,
            run_id: Some("test".into()),
            disable_thinking: false,
        };
        let plan = build_plan(&request).unwrap();
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
    fn mini_submit_is_rejected_only_inside_the_selected_prefix() {
        let command = "  echo COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT";
        assert!(mini_submission_in_prefix(true, command));
        assert!(!mini_submission_in_prefix(false, command));
        assert!(!mini_submission_in_prefix(true, "echo still-working"));
    }

    #[test]
    fn mini_version_probe_accepts_exact_banner_before_config_noise() {
        let output = "This is mini-swe-agent version 2.4.6.\n\
Check the v2 migration guide at https://example.invalid\n\
Loading global config from '/root/.config/mini-swe-agent/.env'";
        assert_eq!(
            probed_version(AgentKind::MiniSweAgent, output, "2.4.6"),
            Some("2.4.6")
        );
        assert_eq!(
            probed_version(AgentKind::MiniSweAgent, output, "2.4.5"),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn mini_python_runtime_finds_the_portable_uv_bundle() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let local = root.path().join(".local");
        let entrypoint = local.join("bin/mini-swe-agent");
        let virtual_env = local.join("share/uv/tools/mini-swe-agent");
        let python_home = local.join("share/uv/python/cpython-3.12.11");
        let python = python_home.join("bin/python3.12");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::create_dir_all(virtual_env.join("bin")).unwrap();
        fs::create_dir_all(virtual_env.join("lib/python3.12/site-packages")).unwrap();
        fs::create_dir_all(python_home.join("lib/python3.12/encodings")).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        let loader = local.join("share/uv/sweeval-system-libs/ld-linux-x86-64.so.2");
        fs::create_dir_all(loader.parent().unwrap()).unwrap();
        fs::write(&loader, "loader").unwrap();
        fs::write(&entrypoint, "#!/bin/sh\nexit 0\n").unwrap();
        fs::write(&python, "python").unwrap();
        symlink(&python, virtual_env.join("bin/python")).unwrap();

        let runtime = mini_python_runtime(&entrypoint).unwrap();
        assert_eq!(runtime.python, python);
        assert_eq!(runtime.python_home.as_deref(), Some(python_home.as_path()));
        assert_eq!(runtime.loader.as_deref(), Some(loader.as_path()));
        assert_eq!(runtime.virtual_env.as_deref(), Some(virtual_env.as_path()));
        let mut command = Command::new(&runtime.python);
        configure_mini_python_environment(&mut command, &runtime).unwrap();
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "PYTHONHOME" && value == Some(python_home.as_os_str())));
        let path = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "PATH").then(|| value.expect("PATH value").to_os_string())
            })
            .expect("PATH override");
        assert_eq!(
            std::env::split_paths(&path).next().unwrap(),
            virtual_env.join("bin")
        );
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

    #[test]
    fn bash_timeout_kills_the_historical_process_group() {
        let workspace = tempfile::tempdir().unwrap();
        let started = Instant::now();
        let (content, is_error, return_code) =
            run_bash("sleep 5", workspace.path(), Duration::from_millis(50)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(is_error);
        assert_eq!(return_code, Some(124));
        assert!(content.contains("timed out"));
    }

    #[test]
    fn wildcard_supports_recursive_style_patterns() {
        assert!(wildcard_match("**/*.rs", "src/lib.rs"));
        assert!(!wildcard_match("*.toml", "src/lib.rs"));
    }
    #[test]
    fn direct_agents_keep_model_credentials_but_claude_tools_do_not() {
        assert!(environment_name_allowed("OPENAI_API_KEY", false));
        assert!(environment_name_allowed("LLM_API_KEY", false));
        assert!(environment_name_allowed("OPENAI_BASE_URL", false));
        assert!(!environment_name_allowed("OPENAI_API_KEY", true));
        assert!(!environment_name_allowed("ANTHROPIC_AUTH_TOKEN", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_BEDROCK", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_VERTEX", true));
        assert!(!environment_name_allowed("CLAUDE_CODE_USE_FOUNDRY", true));
        assert!(!environment_name_allowed("PYTHONPATH", false));
    }
}
