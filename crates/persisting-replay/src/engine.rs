use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::{json, Value};

use crate::adapter::{build_plan, resolve_launch_spec, run, LaunchSpec, RunContext};
use crate::comparison::write_next_action;
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write_json, canonicalize};
use crate::journal::Journal;
use crate::model::{
    AgentKind, AgentResult, AgentStatus, Artifact, ExecutionReport, PlaybackRequest, ReplayFailure,
    ReplayMode, ReplayOutcome, ReplayPhase, ReplayQuality, ReplayResult, RESULT_SCHEMA_VERSION,
};

pub fn execute(request: PlaybackRequest) -> Result<ExecutionReport, ReplayError> {
    let run_id = request
        .run_id
        .clone()
        .unwrap_or_else(|| format!("replay-{}", uuid::Uuid::new_v4().simple()));
    let state_dir = location_hint(&request.state_dir, &run_id);
    let output_dir = location_hint(&request.output_dir, &run_id);
    execute_with_run_id(request, run_id.clone()).map_err(|error| {
        error.with_default_locations(run_id, state_dir, output_dir)
    })
}

fn execute_with_run_id(
    mut request: PlaybackRequest,
    run_id: String,
) -> Result<ExecutionReport, ReplayError> {
    validate(&request)?;
    request.workspace = canonicalize(&request.workspace, ReplayErrorKind::Workspace, "workspace")?;
    request.trajectory = canonicalize(
        &request.trajectory,
        ReplayErrorKind::Configuration,
        "trajectory",
    )?;
    let state_root = absolute_or_current(&request.state_dir)?;
    let output_root = absolute_or_current(&request.output_dir)?;
    let state_dir = state_root.join(&run_id);
    let output_dir = output_root.join(&run_id);
    fs::create_dir_all(&output_root).replay_context(
        ReplayErrorKind::Executor,
        format!("create output root {}", output_root.display()),
    )?;
    fs::create_dir(&output_dir).replay_context(
        ReplayErrorKind::Executor,
        format!("create unique replay output {}", output_dir.display()),
    )?;

    let plan = build_plan(&request)?;
    validate_step_budget(request.mode, request.max_steps, plan.prefix_model_turns)?;
    let launch = resolve_launch_spec(&request)?;
    if let Some(launch) = &launch {
        if launch.version != plan.agent.supported_version() {
            return Err(ReplayError::new(
                ReplayErrorKind::UnsupportedVersion,
                format!(
                    "trajectory version {:?} does not match agent version {:?}",
                    plan.agent.supported_version(),
                    launch.version
                ),
            ));
        }
    }
    atomic_write_json(&output_dir.join("manifest.json"), &plan.public_value())?;

    if let Some(call_id) = Journal::find_ambiguous(&state_dir.join("replay-events.jsonl"))? {
        return Err(ReplayError::new(
            ReplayErrorKind::AmbiguousExecution,
            format!(
                "state contains an uncertain started tool call {call_id:?}; use a new sandbox and run-id to replay from T1"
            ),
        ));
    }
    let mut journal = Journal::open(&state_dir)?;
    journal.append(
        "run_started",
        [
            ("run_id".into(), json!(run_id)),
            ("agent".into(), json!(request.agent.as_str())),
            ("source_sha256".into(), json!(plan.source_sha256)),
            ("after_step".into(), json!(plan.after_step)),
        ],
    )?;
    journal.append(
        "plan_validated",
        [
            ("profile".into(), json!(plan.agent.profile())),
            ("tool_calls".into(), json!(plan.calls().count())),
        ],
    )?;

    let session_id = request.session_id.clone().unwrap_or_else(|| run_id.clone());
    let nonce = format!("__PVISOR_NATIVE_REPLAY_{}__", uuid::Uuid::new_v4().simple());
    let context = RunContext {
        request: &request,
        state_dir: &state_dir,
        output_dir: &output_dir,
        launch: launch.as_ref(),
        session_id: &session_id,
        nonce: &nonce,
    };
    let outcome = match run(&plan, &context, &mut journal) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = journal.append(
                "run_failed",
                [
                    ("category".into(), json!(error.kind.category())),
                    ("message".into(), json!(error.message)),
                ],
            );
            let journal_path = journal.path.clone();
            drop(journal);
            let _ = fs::copy(&journal_path, output_dir.join("replay-events.jsonl"));
            let result = failure_result(
                &request,
                launch.as_ref(),
                &plan,
                run_id,
                state_dir,
                output_dir,
                &error,
            );
            let _ = atomic_write_json(&result.output_dir.join("result.json"), &result);
            return Ok(ExecutionReport {
                exit_code: error.exit_code(),
                result,
            });
        }
    };
    write_next_action_comparison(&request, &plan, &outcome, &output_dir)?;
    journal.append("run_finished", [("status".into(), json!(outcome.status))])?;
    let journal_path = journal.path.clone();
    drop(journal);
    fs::copy(&journal_path, output_dir.join("replay-events.jsonl"))
        .replay_context(ReplayErrorKind::Executor, "copy replay journal to output")?;

    let comparison = read_comparison(&output_dir.join("observation-comparison.json"));
    let exact = comparison
        .iter()
        .filter(|item| item.get("exact").and_then(Value::as_bool) == Some(true))
        .count();
    let different = comparison
        .iter()
        .filter(|item| item.get("exact").and_then(Value::as_bool) == Some(false))
        .count();
    atomic_write_json(
        &output_dir.join("replay-summary.json"),
        &json!({
            "schema_version": "sandbox-playback.summary/v1",
            "agent": request.agent.as_str(),
            "after_step": plan.after_step,
            "replayed_tool_calls": outcome.observations.len(),
            "exact_observations": exact,
            "different_observations": different,
            "comparison_is_gating": false,
            "continuation_status": outcome.status,
            "continued_steps": outcome.continued_steps,
        }),
    )?;

    let artifacts = artifacts(&request, &outcome, &output_dir);
    let result = ReplayResult {
        schema_version: RESULT_SCHEMA_VERSION,
        phase: phase_for_mode(request.mode),
        quality: quality_for_outcome(&outcome),
        agent_status: agent_status_for_outcome(request.mode, &outcome),
        run_id,
        agent: agent_result(&request, launch.as_ref()),
        after_step: plan.after_step,
        replayed_tool_calls: outcome.observations.len(),
        prefix_model_turns: plan.prefix_model_turns,
        continued_steps: outcome.continued_steps,
        state_dir,
        output_dir: output_dir.clone(),
        artifacts,
        failure: None,
        retryable: false,
        metadata: outcome.metadata,
    };
    atomic_write_json(&output_dir.join("result.json"), &result)?;
    Ok(ExecutionReport {
        result,
        exit_code: 0,
    })
}

fn phase_for_mode(mode: ReplayMode) -> ReplayPhase {
    match mode {
        ReplayMode::PrepareOnly => ReplayPhase::Prepared,
        ReplayMode::ReplayOnly => ReplayPhase::Replayed,
        ReplayMode::ReplayAndContinue => ReplayPhase::Continued,
    }
}

fn quality_for_outcome(outcome: &ReplayOutcome) -> ReplayQuality {
    if outcome.observations.iter().any(|observation| {
        observation
            .metadata
            .contains_key("opaque_source_observation")
            || observation.metadata.contains_key("degradation_reason")
    }) {
        ReplayQuality::Degraded
    } else {
        ReplayQuality::Verified
    }
}

fn agent_status_for_outcome(mode: ReplayMode, outcome: &ReplayOutcome) -> AgentStatus {
    if mode != ReplayMode::ReplayAndContinue {
        AgentStatus::NotStarted
    } else if outcome.status == "max_steps" {
        AgentStatus::MaxSteps
    } else {
        AgentStatus::Completed
    }
}

fn failure_result(
    request: &PlaybackRequest,
    launch: Option<&LaunchSpec>,
    plan: &crate::model::ReplayPlan,
    run_id: String,
    state_dir: std::path::PathBuf,
    output_dir: std::path::PathBuf,
    error: &ReplayError,
) -> ReplayResult {
    let artifacts = existing_artifacts(request, &output_dir);
    let replay_completed = artifacts
        .iter()
        .any(|artifact| artifact.role == "reconstructed_native_trajectory");
    ReplayResult {
        schema_version: RESULT_SCHEMA_VERSION,
        phase: if replay_completed {
            ReplayPhase::Replayed
        } else {
            ReplayPhase::Prepared
        },
        quality: ReplayQuality::Verified,
        agent_status: if request.mode == ReplayMode::ReplayAndContinue && replay_completed {
            AgentStatus::Failed
        } else {
            AgentStatus::NotStarted
        },
        run_id,
        agent: agent_result(request, launch),
        after_step: plan.after_step,
        replayed_tool_calls: 0,
        prefix_model_turns: plan.prefix_model_turns,
        continued_steps: 0,
        state_dir,
        output_dir: output_dir.clone(),
        artifacts,
        failure: Some(ReplayFailure {
            category: error.kind.category().into(),
            message: error.to_string(),
        }),
        retryable: error.kind.retryable(),
        metadata: Value::Null,
    }
}

fn write_next_action_comparison(
    request: &PlaybackRequest,
    plan: &crate::model::ReplayPlan,
    outcome: &ReplayOutcome,
    output_dir: &Path,
) -> Result<(), ReplayError> {
    let (Some(original), Some(continued_path)) =
        (&plan.original_next_action, &outcome.continued_path)
    else {
        return Ok(());
    };
    let mut continued_request = request.clone();
    continued_request.trajectory = continued_path.clone();
    let continued_plan = build_plan(&continued_request)?;
    let Some(replayed) = continued_plan.original_next_action else {
        return Ok(());
    };
    write_next_action(
        &output_dir.join("next-action-comparison.json"),
        original,
        &replayed,
    )
}

fn validate(request: &PlaybackRequest) -> Result<(), ReplayError> {
    if request.after_step == 0 {
        return Err(ReplayError::configuration(
            "after_step must be a positive complete batch ordinal",
        ));
    }
    if request.max_steps == Some(0) {
        return Err(ReplayError::configuration("max_steps must be positive"));
    }
    if !request.workspace.is_dir() {
        return Err(ReplayError::new(
            ReplayErrorKind::Workspace,
            format!(
                "workspace is not a directory: {}",
                request.workspace.display()
            ),
        ));
    }
    if !request.trajectory.is_file() {
        return Err(ReplayError::configuration(format!(
            "trajectory does not exist: {}",
            request.trajectory.display()
        )));
    }
    if let Some(run_id) = &request.run_id {
        if run_id.is_empty()
            || !run_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(ReplayError::configuration(
                "run_id may contain only letters, digits, '-' and '_'",
            ));
        }
    }
    if let Some(session_id) = &request.session_id {
        if session_id.is_empty()
            || session_id
                .chars()
                .any(|character| matches!(character, '\0' | '\r' | '\n'))
        {
            return Err(ReplayError::configuration(
                "session_id must be non-empty and contain no NUL, CR, or LF",
            ));
        }
    }
    if request.agent != AgentKind::ClaudeCode && !request.disallowed_tools.is_empty() {
        return Err(ReplayError::configuration(
            "disallowed_tools is supported only for claude-code",
        ));
    }
    let mut seen = BTreeSet::new();
    for tool in &request.disallowed_tools {
        if tool.is_empty()
            || !tool.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        {
            return Err(ReplayError::configuration(format!(
                "invalid disallowed tool name {tool:?}"
            )));
        }
        if !seen.insert(tool) {
            return Err(ReplayError::configuration(format!(
                "duplicate disallowed tool {tool:?}"
            )));
        }
    }
    Ok(())
}

fn validate_step_budget(
    mode: ReplayMode,
    max_steps: Option<usize>,
    prefix_steps: usize,
) -> Result<(), ReplayError> {
    let Some(max_steps) = max_steps else {
        return Ok(());
    };
    match mode {
        ReplayMode::PrepareOnly => Ok(()),
        ReplayMode::ReplayOnly if max_steps < prefix_steps => Err(ReplayError::configuration(
            format!(
                "max_steps {max_steps} is smaller than the selected replay prefix of {prefix_steps} steps"
            ),
        )),
        ReplayMode::ReplayAndContinue if max_steps <= prefix_steps => {
            Err(ReplayError::configuration(format!(
                "max_steps {max_steps} leaves no live step after the selected replay prefix of {prefix_steps} steps"
            )))
        }
        _ => Ok(()),
    }
}

fn absolute_or_current(path: &Path) -> Result<std::path::PathBuf, ReplayError> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .replay_context(ReplayErrorKind::Configuration, "read current directory")?
            .join(path))
    }
}

fn location_hint(path: &Path, run_id: &str) -> std::path::PathBuf {
    if path.is_absolute() {
        path.join(run_id)
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path).join(run_id))
            .unwrap_or_else(|_| path.join(run_id))
    }
}

fn read_comparison(path: &Path) -> Vec<Value> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn agent_result(request: &PlaybackRequest, launch: Option<&LaunchSpec>) -> AgentResult {
    AgentResult {
        kind: request.agent.as_str().into(),
        version: launch
            .map(|launch| launch.version.clone())
            .unwrap_or_else(|| request.agent.supported_version().into()),
        entrypoint: launch.map(|launch| launch.entrypoint.clone()),
        launch_source: launch
            .map(|launch| launch.source.clone())
            .unwrap_or_else(|| "prepare_only".into()),
        disallowed_tools: request.disallowed_tools.clone(),
    }
}

fn artifacts(
    request: &PlaybackRequest,
    outcome: &ReplayOutcome,
    output_dir: &Path,
) -> Vec<Artifact> {
    let mut artifacts = vec![
        artifact(
            "playback_plan",
            "sandbox-playback/plan-v1",
            output_dir.join("manifest.json"),
        ),
        artifact(
            "replay_events",
            "sandbox-playback/events-v1",
            output_dir.join("replay-events.jsonl"),
        ),
        artifact(
            "replay_summary",
            "sandbox-playback/summary-v1",
            output_dir.join("replay-summary.json"),
        ),
    ];
    let native_format = match request.agent {
        AgentKind::ClaudeCode => "claude-code/native-jsonl-2.1.220",
        AgentKind::MiniSweAgent => "mini-swe-agent/native-json-2.4.6",
        AgentKind::Openhands => "openhands/native-json-0.53.0",
        AgentKind::SweAgent => "swe-agent/native-traj-1.1.0",
    };
    if let Some(path) = &outcome.reconstructed_path {
        artifacts.push(artifact(
            "reconstructed_native_trajectory",
            native_format,
            path.clone(),
        ));
    }
    if let Some(path) = &outcome.continued_path {
        artifacts.push(artifact(
            "continued_native_trajectory",
            native_format,
            path.clone(),
        ));
    }
    for (role, format, name) in [
        (
            "observation_comparison",
            "sandbox-playback/comparison-v1",
            "observation-comparison.json",
        ),
        (
            "next_action_comparison",
            "sandbox-playback/next-action-v2",
            "next-action-comparison.json",
        ),
    ] {
        let path = output_dir.join(name);
        if path.is_file() {
            artifacts.push(artifact(role, format, path));
        }
    }
    if output_dir.join("logs").is_dir() {
        artifacts.push(artifact(
            "agent_logs",
            "sandbox-playback/log-directory-v1",
            output_dir.join("logs"),
        ));
    }
    artifacts
}

fn existing_artifacts(request: &PlaybackRequest, output_dir: &Path) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    for (role, format, path) in [
        (
            "playback_plan",
            "sandbox-playback/plan-v1",
            output_dir.join("manifest.json"),
        ),
        (
            "replay_events",
            "sandbox-playback/events-v1",
            output_dir.join("replay-events.jsonl"),
        ),
        (
            "replay_summary",
            "sandbox-playback/summary-v1",
            output_dir.join("replay-summary.json"),
        ),
        (
            "observation_comparison",
            "sandbox-playback/comparison-v1",
            output_dir.join("observation-comparison.json"),
        ),
        (
            "next_action_comparison",
            "sandbox-playback/next-action-v2",
            output_dir.join("next-action-comparison.json"),
        ),
    ] {
        if path.is_file() {
            artifacts.push(artifact(role, format, path));
        }
    }

    let native_format = match request.agent {
        AgentKind::ClaudeCode => "claude-code/native-jsonl-2.1.220",
        AgentKind::MiniSweAgent => "mini-swe-agent/native-json-2.4.6",
        AgentKind::Openhands => "openhands/native-json-0.53.0",
        AgentKind::SweAgent => "swe-agent/native-traj-1.1.0",
    };
    let native_paths: &[(&str, &str)] = match request.agent {
        AgentKind::ClaudeCode => &[
            ("reconstructed_native_trajectory", "native/reconstructed-prefix.jsonl"),
            ("continued_native_trajectory", "native/continued-session.jsonl"),
        ],
        AgentKind::MiniSweAgent => &[
            ("reconstructed_native_trajectory", "native/prepared-prefix.json"),
            ("continued_native_trajectory", "native/continued-trajectory.json"),
        ],
        AgentKind::Openhands => &[
            (
                "reconstructed_native_trajectory",
                "native/prepared-replay-events.json",
            ),
            ("continued_native_trajectory", "native/continued-trajectory.json"),
        ],
        AgentKind::SweAgent => &[
            ("reconstructed_native_trajectory", "native/prepared-prefix.traj"),
            ("continued_native_trajectory", "native/continued-trajectory.json"),
        ],
    };
    for (role, relative) in native_paths {
        let path = output_dir.join(relative);
        if path.is_file() {
            artifacts.push(artifact(role, native_format, path));
        }
    }
    if output_dir.join("logs").is_dir() {
        artifacts.push(artifact(
            "agent_logs",
            "sandbox-playback/log-directory-v1",
            output_dir.join("logs"),
        ));
    }
    artifacts
}

fn artifact(role: &str, format: &str, path: std::path::PathBuf) -> Artifact {
    Artifact {
        role: role.into(),
        format: format.into(),
        path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn validation_errors_keep_resolved_run_locations() {
        let request = PlaybackRequest {
            agent: AgentKind::ClaudeCode,
            trajectory: std::path::PathBuf::from("/missing/trajectory.jsonl"),
            after_step: 1,
            workspace: std::path::PathBuf::from("/missing/workspace"),
            state_dir: std::path::PathBuf::from("/state"),
            output_dir: std::path::PathBuf::from("/output"),
            agent_entrypoint: None,
            agent_runtime: None,
            disallowed_tools: Vec::new(),
            trajectory_assets: None,
            session_id: None,
            max_steps: None,
            mode: ReplayMode::PrepareOnly,
            allow_stale_observations: false,
            run_id: Some("replay-1".into()),
            disable_thinking: false,
        };

        let error = execute(request).unwrap_err();
        let (run_id, state_dir, output_dir) = error.locations().unwrap();
        assert_eq!(run_id, "replay-1");
        assert_eq!(state_dir, Path::new("/state/replay-1"));
        assert_eq!(output_dir, Path::new("/output/replay-1"));
    }

    #[test]
    fn stale_source_observations_degrade_result_quality() {
        let outcome = ReplayOutcome {
            status: "replayed".into(),
            reconstructed_path: None,
            continued_path: None,
            observations: vec![crate::model::FreshObservation {
                call_id: "call-1".into(),
                content: Value::Null,
                is_error: false,
                return_code: Some(0),
                duration_ms: 0,
                truncated: false,
                metadata: BTreeMap::from([(
                    "degradation_reason".into(),
                    json!("stale_source_observation"),
                )]),
            }],
            continued_steps: 0,
            metadata: Value::Null,
        };

        assert_eq!(quality_for_outcome(&outcome), ReplayQuality::Degraded);
    }

    #[test]
    fn total_step_budget_is_checked_before_replay() {
        assert!(validate_step_budget(ReplayMode::ReplayOnly, Some(3), 3).is_ok());
        assert!(validate_step_budget(ReplayMode::ReplayOnly, Some(2), 3).is_err());
        assert!(validate_step_budget(ReplayMode::ReplayAndContinue, Some(3), 3).is_err());
        assert!(validate_step_budget(ReplayMode::ReplayAndContinue, Some(4), 3).is_ok());
        assert!(validate_step_budget(ReplayMode::PrepareOnly, Some(1), 3).is_ok());
    }
}
