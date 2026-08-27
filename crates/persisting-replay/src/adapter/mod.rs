use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod claude_code;
mod mini_swe_agent;
mod openhands;
mod pi_agent;
mod runtime;
mod swe_agent;

use serde_json::{Value, json};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write, atomic_write_json, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, AgentKind, FreshObservation, PlaybackRequest, ReplayMode, ReplayOutcome,
    ReplayPlan,
};
use crate::process::{ProcessSpec, run_process};
pub(crate) use runtime::{LaunchSpec, resolve_launch_spec};
use runtime::{
    configure_mini_python_environment, mini_python_library_path, mini_python_runtime,
    pi_node_runtime,
};

const MAX_TOOL_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub struct RunContext<'a> {
    pub request: &'a PlaybackRequest,
    pub state_dir: &'a Path,
    pub output_dir: &'a Path,
    pub launch: Option<&'a LaunchSpec>,
    pub session_id: &'a str,
    pub nonce: &'a str,
}

pub fn build_plan(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    match request.agent {
        AgentKind::ClaudeCode => claude_code::build(request),
        AgentKind::MiniSweAgent => mini_swe_agent::build(request),
        AgentKind::Openhands => openhands::build(request),
        AgentKind::PiAgent => pi_agent::build(request),
        AgentKind::SweAgent => swe_agent::build(request),
    }
}

pub fn run(
    plan: &AdapterPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    match plan {
        AdapterPlan::ClaudeCode(plan) => claude_code::execute(plan, context, journal),
        AdapterPlan::MiniSweAgent(plan) => mini_swe_agent::execute(plan, context, journal),
        AdapterPlan::Openhands(plan) => openhands::execute(plan, context, journal),
        AdapterPlan::PiAgent(plan) => pi_agent::execute(plan, context, journal),
        AdapterPlan::SweAgent(plan) => swe_agent::execute(plan, context, journal),
    }
}

fn check_boundary(after_step: usize, complete: usize) -> Result<(), ReplayError> {
    if after_step == 0 || after_step > complete {
        return Err(ReplayError::trajectory(format!(
            "requested after-step {after_step}, trajectory has {complete} complete batches"
        )));
    }
    Ok(())
}

fn prepared_outcome(path: PathBuf, request: &PlaybackRequest) -> ReplayOutcome {
    ReplayOutcome {
        status: "prepared".into(),
        reconstructed_path: Some(path),
        continued_path: None,
        observations: Vec::new(),
        continued_steps: 0,
        metadata: with_boundary_user_prompt_metadata(
            json!({"replay_only_execution": false}),
            request,
            false,
        ),
    }
}

pub(super) fn with_boundary_user_prompt_metadata(
    mut metadata: Value,
    request: &PlaybackRequest,
    injected: bool,
) -> Value {
    let prompt = request.boundary_user_prompt();
    let mut detail = json!({
        "requested": prompt.is_some(),
        "injected": injected,
        "injection_count": usize::from(injected),
    });
    if let Some(prompt) = prompt {
        detail["sha256"] = json!(sha256(prompt.as_bytes()));
        detail["length"] = json!(prompt.chars().count());
        if injected {
            detail["position"] = json!("after_boundary_observation");
        } else {
            detail["reason"] = json!(match request.mode {
                ReplayMode::PrepareOnly => "prepare_only",
                ReplayMode::ReplayOnly => "replay_only",
                ReplayMode::ReplayAndContinue => "not_injected",
            });
        }
    }
    metadata
        .as_object_mut()
        .expect("replay outcome metadata must be an object")
        .insert("boundary_user_prompt".into(), detail);
    metadata
}

fn run_sdk_bridge(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
    agent: AgentKind,
) -> Result<ReplayOutcome, ReplayError> {
    let launch = context
        .launch
        .ok_or_else(|| ReplayError::continuation("SDK continuation has no launch spec"))?;
    let native_dir = context.output_dir.join("native");
    let logs_dir = context.output_dir.join("logs");
    fs::create_dir_all(&native_dir)
        .replay_context(ReplayErrorKind::Executor, "create native output directory")?;
    fs::create_dir_all(&logs_dir)
        .replay_context(ReplayErrorKind::Executor, "create Agent log directory")?;

    let runner_result = context
        .state_dir
        .join(format!("{}-runner-result.json", agent.as_str()));
    let mode = match context.request.mode {
        ReplayMode::ReplayOnly => "replay_only",
        ReplayMode::ReplayAndContinue => "replay_and_continue",
        ReplayMode::PrepareOnly => {
            return Err(ReplayError::new(
                ReplayErrorKind::Internal,
                "prepare-only unexpectedly started an SDK runner",
            ));
        }
    };
    let (
        program,
        bridge_source,
        bridge_name,
        request_value,
        reconstructed,
        continued,
        observations_path,
    ) = match agent {
        AgentKind::MiniSweAgent => {
            let source = context.state_dir.join("mini-source.json");
            let reconstructed = native_dir.join("reconstructed-trajectory.json");
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
                include_str!("../../assets/mini_swe_agent_runner.py"),
                "mini-swe-agent-runner.py",
                json!({
                    "source": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "observations": observations,
                    "result": runner_result,
                    "mode": mode,
                    "workspace": context.request.workspace,
                    "after_step": plan.after_step,
                    "max_steps": context.request.max_steps,
                    "session_id": context.session_id,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                }),
                reconstructed,
                continued,
                Some(observations),
            )
        }
        AgentKind::SweAgent => {
            let source = native_dir.join("continuation-source.traj");
            let run_output = native_dir.join("swe-agent-run");
            let reconstructed = native_dir.join("reconstructed-trajectory.traj");
            let continued = native_dir.join("continued-trajectory.traj");
            atomic_write_json(&source, &plan.native)?;
            (
                launch.entrypoint.clone(),
                include_str!("../../assets/swe_agent_runner.py"),
                "swe-agent-runner.py",
                json!({
                    "trajectory": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "trajectory_assets": context.request.trajectory_assets,
                    "after_step": plan.after_step,
                    "max_steps": context.request.max_steps,
                    "mode": mode,
                    "result": runner_result,
                    "workspace": context.request.workspace,
                    "output_dir": run_output,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                }),
                reconstructed,
                continued,
                None,
            )
        }
        AgentKind::PiAgent => {
            let source = context.state_dir.join("pi-source.json");
            let reconstructed = native_dir.join("reconstructed-events.jsonl");
            let continued = native_dir.join("continued-events.jsonl");
            let observations = context.state_dir.join("pi-fresh-observations.json");
            atomic_write_json(&source, &plan.native)?;
            let runtime = pi_node_runtime(&launch.entrypoint)?;
            (
                runtime.node,
                include_str!("../../assets/pi_agent_runner.mjs"),
                "pi-agent-runner.mjs",
                json!({
                    "source": source,
                    "reconstructed": reconstructed,
                    "continued": continued,
                    "observations": observations,
                    "result": runner_result,
                    "mode": mode,
                    "workspace": context.request.workspace,
                    "after_step": plan.after_step,
                    "prefix_model_turns": plan.prefix_model_turns,
                    "max_steps": context.request.max_steps,
                    "session_id": context.session_id,
                    "boundary_user_prompt": context.request.boundary_user_prompt(),
                    "disable_thinking": context.request.disable_thinking,
                    "package_json": runtime.package_json,
                    "config_dir": context.state_dir.join("pi-config"),
                    "session_dir": context.state_dir.join("pi-sessions"),
                }),
                reconstructed,
                continued,
                Some(observations),
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
    journal.append("continuation_started", std::iter::empty())?;
    let log = logs_dir.join(format!("{}.log", agent.as_str()));
    let output = run_process(ProcessSpec {
        command,
        stdin: None,
        timeout: Duration::from_secs(24 * 60 * 60),
        termination_grace: Duration::from_secs(2),
        pipe_grace: Duration::from_millis(250),
        retained_bytes: MAX_TOOL_OUTPUT_BYTES / 2,
        log_path: log.clone(),
    })
    .map_err(|error| ReplayError::new(ReplayErrorKind::Continuation, error.message))?;
    if !output.status.success() {
        let mut rendered = String::from_utf8_lossy(&output.stdout_tail).into_owned();
        if !output.stderr_tail.is_empty() {
            rendered.push('\n');
            rendered.push_str(&String::from_utf8_lossy(&output.stderr_tail));
        }
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

    let runner: Value = serde_json::from_slice(&read_regular_file(&runner_result)?)
        .replay_context(
            ReplayErrorKind::Continuation,
            "parse SDK replay runner result",
        )?;
    let expected_phase = if context.request.mode == ReplayMode::ReplayOnly {
        "replayed"
    } else {
        "continued"
    };
    if runner.get("phase").and_then(Value::as_str) != Some(expected_phase)
        || runner.get("replayed_steps").and_then(Value::as_u64) != Some(plan.after_step as u64)
    {
        return Err(ReplayError::continuation(format!(
            "{} runner returned an invalid replay boundary",
            agent.as_str()
        )));
    }
    let runner_continued_steps = runner
        .get("continued_steps")
        .and_then(Value::as_u64)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted continued_steps"))?
        as usize;
    let runner_agent_status = runner
        .get("agent_status")
        .and_then(Value::as_str)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted agent_status"))?;
    let prompt_injected = runner
        .get("boundary_user_prompt_injected")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            ReplayError::continuation("SDK runner omitted boundary_user_prompt_injected")
        })?;
    let expected_prompt_injected = context.request.mode == ReplayMode::ReplayAndContinue
        && context.request.boundary_user_prompt().is_some();
    if prompt_injected != expected_prompt_injected {
        return Err(ReplayError::continuation(format!(
            "{} runner reported an invalid boundary user prompt injection state",
            agent.as_str()
        )));
    }
    let status_is_valid = match context.request.mode {
        ReplayMode::ReplayOnly => {
            runner_agent_status == "not_started" && runner_continued_steps == 0
        }
        ReplayMode::ReplayAndContinue => {
            matches!(runner_agent_status, "completed" | "max_steps")
                && context.request.max_steps.is_none_or(|max_steps| {
                    plan.prefix_model_turns + runner_continued_steps <= max_steps
                })
        }
        ReplayMode::PrepareOnly => false,
    };
    if !status_is_valid {
        return Err(ReplayError::continuation(format!(
            "{} runner returned an invalid terminal status or step count",
            agent.as_str()
        )));
    }
    let runner_trajectory = runner
        .get("trajectory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| ReplayError::continuation("SDK runner omitted trajectory"))?;
    let expected_trajectory = if context.request.mode == ReplayMode::ReplayOnly {
        &reconstructed
    } else {
        &continued
    };
    if runner_trajectory != *expected_trajectory || !runner_trajectory.is_file() {
        return Err(ReplayError::continuation(format!(
            "{} runner produced an unexpected trajectory path",
            agent.as_str()
        )));
    }

    let (observations, continued_steps) =
        if matches!(agent, AgentKind::MiniSweAgent | AgentKind::PiAgent) {
            let raw_observations: Vec<Value> = serde_json::from_slice(&read_regular_file(
                observations_path.as_ref().expect("SDK observations path"),
            )?)
            .replay_context(
                ReplayErrorKind::Trajectory,
                format!("parse {} fresh observations", agent.as_str()),
            )?;
            if raw_observations.len() != plan.calls().count() {
                return Err(ReplayError::trajectory(format!(
                    "{} output lost replayed observations",
                    agent.as_str()
                )));
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
            let measured = if agent == AgentKind::MiniSweAgent {
                let continued_value: Value =
                    serde_json::from_slice(&read_regular_file(&runner_trajectory)?)
                        .replay_context(
                            ReplayErrorKind::Trajectory,
                            "parse continued mini-swe-agent trajectory",
                        )?;
                continued_value["messages"]
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
                    .unwrap_or_default()
                    .saturating_sub(plan.after_step)
            } else {
                let raw = read_regular_file(&runner_trajectory)?;
                let turns = String::from_utf8_lossy(&raw)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter(|event| event.get("type").and_then(Value::as_str) == Some("turn_end"))
                    .count();
                turns.saturating_sub(plan.prefix_model_turns)
            };
            if measured != runner_continued_steps {
                return Err(ReplayError::trajectory(format!(
                    "{} runner result disagrees with its trajectory",
                    agent.as_str()
                )));
            }
            (observations, measured)
        } else {
            let replayed: Value = serde_json::from_slice(&read_regular_file(&runner_trajectory)?)
                .replay_context(
                ReplayErrorKind::Trajectory,
                "parse SWE-agent replay runner trajectory",
            )?;
            let steps = replayed["trajectory"].as_array().ok_or_else(|| {
                ReplayError::trajectory("continued SWE-agent trajectory is invalid")
            })?;
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
            if continued_steps != runner_continued_steps {
                return Err(ReplayError::trajectory(
                    "SWE-agent runner result disagrees with its trajectory",
                ));
            }
            (observations, continued_steps)
        };
    if context.request.mode == ReplayMode::ReplayAndContinue && continued_steps == 0 {
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
        status: if context.request.mode == ReplayMode::ReplayOnly {
            "replayed".into()
        } else {
            runner_agent_status.into()
        },
        reconstructed_path: Some(reconstructed),
        continued_path: (context.request.mode == ReplayMode::ReplayAndContinue)
            .then_some(continued),
        observations,
        continued_steps,
        metadata: with_boundary_user_prompt_metadata(
            json!({"sdk_bridge": bridge_name}),
            context.request,
            prompt_injected,
        ),
    })
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
    !(claude_provider_override
        || matches!(rendered, "PYTHONHOME" | "PYTHONPATH" | "VIRTUAL_ENV")
        || strip_credentials && credential)
}

#[cfg(test)]
mod tests {
    use super::environment_name_allowed;

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
