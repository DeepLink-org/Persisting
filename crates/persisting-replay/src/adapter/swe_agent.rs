use std::path::Path;

use serde_json::{Value, json};

use super::runtime::safe_relative;
use super::{RunContext, check_boundary, prepared_outcome, run_sdk_bridge};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};
use crate::io::{atomic_write_json, canonicalize, read_regular_file, sha256};
use crate::journal::Journal;
use crate::model::{
    AdapterPlan, AgentKind, PlaybackRequest, ReplayMode, ReplayOutcome, ReplayPlan, ToolBatch,
    ToolCall,
};

pub(super) fn build(request: &PlaybackRequest) -> Result<AdapterPlan, ReplayError> {
    build_swe_plan(request).map(AdapterPlan::SweAgent)
}

pub(super) fn execute(
    plan: &ReplayPlan,
    context: &RunContext<'_>,
    journal: &mut Journal,
) -> Result<ReplayOutcome, ReplayError> {
    run_swe(plan, context, journal)
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
        [(
            "prepared_only".into(),
            json!(context.request.mode == ReplayMode::PrepareOnly),
        )],
    )?;
    if context.request.mode == ReplayMode::PrepareOnly {
        return Ok(prepared_outcome(path, context.request));
    }
    run_sdk_bridge(plan, context, journal, AgentKind::SweAgent)
}
