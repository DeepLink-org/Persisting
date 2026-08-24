use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use persisting_replay::{
    execute, AgentKind, AgentStatus, PlaybackRequest, ReplayMode, ReplayPhase,
};
use serde_json::{json, Value};

fn crate_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn run_fake(kind: &str, runner: &str, request: &Path, live_marker: &Path) {
    let output = Command::new("python3")
        .arg(crate_path("tests/fixtures/fake_agent_runtime.py"))
        .arg(kind)
        .arg(crate_path(runner))
        .arg(request)
        .env("FAKE_LIVE_MARKER", live_marker)
        .env("OPENAI_API_KEY", "fake")
        .env("OPENAI_BASE_URL", "http://127.0.0.1.invalid")
        .env("MODEL_NAME", "fake-model")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fake runner failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn write_fake_openhands_entrypoint(path: &Path) {
    fs::write(
        path,
        r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

if len(sys.argv) > 1 and sys.argv[1] == "-c":
    print("0.53.0")
    raise SystemExit(0)

prepared = pathlib.Path(os.environ["REPLAY_TRAJECTORY_PATH"])
continued = pathlib.Path(os.environ["SAVE_TRAJECTORY_PATH"])
events = json.loads(prepared.read_text(encoding="utf-8"))
actions = [event for event in events if event.get("source") == "agent" and event.get("action") == "run"]
next_id = max(event["id"] for event in events) + 1
for action in actions:
    if not any(event.get("cause") == action["id"] for event in events):
        events.append({
            "id": next_id,
            "source": "environment",
            "observation": "run",
            "cause": action["id"],
            "message": "fresh replay observation",
            "args": {"command": action.get("args", {}).get("command", ""), "metadata": {"exit_code": 0}},
        })
        next_id += 1

limit = int(os.environ["MAX_ITERATIONS"])
while len(actions) < limit:
    pathlib.Path("live-marker").write_text("live\n", encoding="utf-8")
    action_id = next_id
    next_id += 1
    action = {"id": action_id, "source": "agent", "action": "run", "args": {"command": "echo live"}}
    observation = {
        "id": next_id,
        "source": "environment",
        "observation": "run",
        "cause": action_id,
        "message": "fresh live observation",
        "args": {"command": "echo live", "metadata": {"exit_code": 0}},
    }
    next_id += 1
    events.extend([action, observation])
    actions.append(action)

continued.parent.mkdir(parents=True, exist_ok=True)
continued.write_text(json.dumps(events), encoding="utf-8")
if pathlib.Path("fatal-mode").exists():
    print("Error while running the agent", file=sys.stderr)
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

#[cfg(unix)]
fn openhands_request(root: &Path, mode: ReplayMode, max_steps: usize) -> PlaybackRequest {
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let trajectory = root.join("openhands-trajectory.json");
    fs::write(
        &trajectory,
        serde_json::to_vec(&json!([
            {"id": 0, "source": "user", "action": "message", "args": {"content": "fix it"}},
            {"id": 1, "source": "agent", "action": "run", "args": {"command": "pwd"}},
            {
                "id": 2,
                "source": "environment",
                "observation": "run",
                "cause": 1,
                "message": "old observation",
                "args": {"command": "pwd", "metadata": {"exit_code": 0}}
            }
        ]))
        .unwrap(),
    )
    .unwrap();
    let entrypoint = root.join("fake-openhands");
    write_fake_openhands_entrypoint(&entrypoint);
    PlaybackRequest {
        agent: AgentKind::Openhands,
        trajectory,
        after_step: 1,
        workspace,
        state_dir: root.join("state"),
        output_dir: root.join("output"),
        agent_entrypoint: Some(entrypoint),
        agent_runtime: None,
        disallowed_tools: Vec::new(),
        trajectory_assets: None,
        session_id: None,
        max_steps: Some(max_steps),
        mode,
        allow_stale_observations: false,
        run_id: Some("contract".into()),
        disable_thinking: false,
        boundary_user_prompt: None,
    }
}

#[test]
fn mini_replay_only_executes_prefix_without_live_model() {
    let temporary = tempfile::tempdir().unwrap();
    let historical_marker = temporary.path().join("historical-marker");
    let live_marker = temporary.path().join("live-marker");
    let source = temporary.path().join("source.json");
    fs::write(
        &source,
        serde_json::to_vec(&json!({
            "info": {
                "config": {
                    "model": {}, "environment": {}, "agent": {}
                }
            },
            "messages": [
                {"role": "system", "content": "system", "extra": {}},
                {
                    "role": "assistant",
                    "content": "historical action",
                    "extra": {
                        "response": {},
                        "actions": [{
                            "tool_call_id": "call-1",
                            "marker": historical_marker,
                        }]
                    }
                },
                {"role": "tool", "content": "old observation", "extra": {}}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let request_path = temporary.path().join("request.json");
    let result_path = temporary.path().join("result.json");
    let observations = temporary.path().join("observations.json");
    let reconstructed = temporary.path().join("reconstructed.json");
    let continued = temporary.path().join("continued.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&json!({
            "source": source,
            "reconstructed": reconstructed,
            "continued": continued,
            "observations": observations,
            "result": result_path,
            "workspace": temporary.path(),
            "after_step": 1,
            "max_steps": 1,
            "session_id": "session",
            "mode": "replay_only",
            "boundary_user_prompt": "must not be injected"
        }))
        .unwrap(),
    )
    .unwrap();

    run_fake(
        "mini",
        "assets/mini_swe_agent_runner.py",
        &request_path,
        &live_marker,
    );

    let result: Value = serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
    assert!(historical_marker.is_file());
    assert!(!live_marker.exists());
    assert_eq!(result["phase"], "replayed");
    assert_eq!(result["replayed_steps"], 1);
    assert_eq!(result["continued_steps"], 0);
    assert_eq!(result["boundary_user_prompt_injected"], false);
    assert!(reconstructed.is_file());
    assert!(!continued.exists());
}

#[test]
fn mini_boundary_prompt_is_persisted_after_the_fresh_observation() {
    let temporary = tempfile::tempdir().unwrap();
    let historical_marker = temporary.path().join("historical-marker");
    let live_marker = temporary.path().join("live-marker");
    let source = temporary.path().join("source.json");
    fs::write(
        &source,
        serde_json::to_vec(&json!({
            "info": {"config": {"model": {}, "environment": {}, "agent": {}}},
            "messages": [
                {"role": "system", "content": "system", "extra": {}},
                {
                    "role": "assistant",
                    "content": "historical action",
                    "extra": {
                        "response": {},
                        "actions": [{"tool_call_id": "call-1", "marker": historical_marker}]
                    }
                },
                {"role": "tool", "content": "old observation", "extra": {}}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let request_path = temporary.path().join("request.json");
    let result_path = temporary.path().join("result.json");
    let continued = temporary.path().join("continued.json");
    fs::write(
        &request_path,
        serde_json::to_vec(&json!({
            "source": source,
            "reconstructed": temporary.path().join("reconstructed.json"),
            "continued": continued,
            "observations": temporary.path().join("observations.json"),
            "result": result_path,
            "workspace": temporary.path(),
            "after_step": 1,
            "max_steps": 2,
            "session_id": "session",
            "mode": "replay_and_continue",
            "boundary_user_prompt": "review O-prime N"
        }))
        .unwrap(),
    )
    .unwrap();

    run_fake(
        "mini",
        "assets/mini_swe_agent_runner.py",
        &request_path,
        &live_marker,
    );

    let result: Value = serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
    let trajectory: Value = serde_json::from_slice(&fs::read(continued).unwrap()).unwrap();
    let messages = trajectory["messages"].as_array().unwrap();
    let prompt_index = messages
        .iter()
        .position(|message| message["content"] == "review O-prime N")
        .unwrap();
    assert_eq!(messages[prompt_index]["role"], "user");
    assert_eq!(messages[prompt_index - 1]["content"], "fresh observation");
    assert_eq!(result["boundary_user_prompt_injected"], true);
}

#[test]
fn swe_max_steps_caps_total_actions() {
    let temporary = tempfile::tempdir().unwrap();
    let live_marker = temporary.path().join("live-marker");
    let source = temporary.path().join("source.traj");
    fs::write(
        &source,
        serde_json::to_vec(&json!({
            "replay_config": {
                "agent": {"type": "default", "model": {}},
                "env": {},
                "problem_statement": {"type": "text", "text": "problem", "id": "fake"}
            },
            "history": [
                {"role": "assistant", "content": "historical action"},
                {"role": "user", "content": "old observation"}
            ],
            "trajectory": [
                {"action": "historical action", "observation": "old observation"}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let request_path = temporary.path().join("request.json");
    let result_path = temporary.path().join("result.json");
    let reconstructed = temporary.path().join("reconstructed.traj");
    let continued = temporary.path().join("continued.traj");
    fs::write(
        &request_path,
        serde_json::to_vec(&json!({
            "trajectory": source,
            "reconstructed": reconstructed,
            "continued": continued,
            "result": result_path,
            "workspace": temporary.path(),
            "output_dir": temporary.path().join("agent-output"),
            "after_step": 1,
            "max_steps": 3,
            "mode": "replay_and_continue",
            "boundary_user_prompt": "review O-prime N"
        }))
        .unwrap(),
    )
    .unwrap();

    run_fake(
        "swe",
        "assets/swe_agent_runner.py",
        &request_path,
        &live_marker,
    );

    let result: Value = serde_json::from_slice(&fs::read(result_path).unwrap()).unwrap();
    assert_eq!(result["phase"], "continued");
    assert_eq!(result["agent_status"], "max_steps");
    assert_eq!(result["replayed_steps"], 1);
    assert_eq!(result["continued_steps"], 2);
    assert_eq!(result["boundary_user_prompt_injected"], true);
    assert_eq!(fs::read_to_string(live_marker).unwrap().lines().count(), 2);
    assert!(reconstructed.is_file());
    assert!(continued.is_file());
    let trajectory: Value = serde_json::from_slice(&fs::read(continued).unwrap()).unwrap();
    assert!(trajectory["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| { message["role"] == "user" && message["content"] == "review O-prime N" }));
}

#[cfg(unix)]
#[test]
fn openhands_replay_only_stops_at_boundary() {
    let temporary = tempfile::tempdir().unwrap();
    let report = execute(openhands_request(
        temporary.path(),
        ReplayMode::ReplayOnly,
        1,
    ))
    .unwrap();

    assert_eq!(report.exit_code, 0);
    assert_eq!(report.result.phase, ReplayPhase::Replayed);
    assert_eq!(report.result.agent_status, AgentStatus::NotStarted);
    assert_eq!(report.result.replayed_tool_calls, 1);
    assert_eq!(report.result.continued_steps, 0);
    assert!(!temporary.path().join("workspace/live-marker").exists());
    assert!(report.result.artifacts.iter().any(|artifact| {
        artifact.role == "reconstructed_native_trajectory"
            && artifact
                .path
                .ends_with("native/reconstructed-trajectory.json")
    }));
}

#[cfg(unix)]
#[test]
fn openhands_boundary_prompt_is_queued_after_the_replay_prefix() {
    let temporary = tempfile::tempdir().unwrap();
    let mut request = openhands_request(temporary.path(), ReplayMode::ReplayAndContinue, 2);
    request.boundary_user_prompt = Some("review O-prime N".into());

    let report = execute(request).unwrap();

    assert_eq!(report.exit_code, 0);
    let prepared: Value = serde_json::from_slice(
        &fs::read(
            report
                .result
                .output_dir
                .join("native/prepared-replay-events.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let last = prepared.as_array().unwrap().last().unwrap();
    assert_eq!(last["source"], "user");
    assert_eq!(last["action"], "message");
    assert_eq!(last["args"]["content"], "review O-prime N");
    assert_eq!(
        report.result.metadata["boundary_user_prompt"]["injected"],
        true
    );
    assert!(!report
        .result
        .metadata
        .to_string()
        .contains("review O-prime N"));
}

#[cfg(unix)]
#[test]
fn openhands_zero_exit_fatal_status_is_a_failed_result_with_trajectory() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("fatal-mode"), "1\n").unwrap();
    let report = execute(openhands_request(
        temporary.path(),
        ReplayMode::ReplayAndContinue,
        2,
    ))
    .unwrap();

    assert_ne!(report.exit_code, 0);
    assert_eq!(report.result.agent_status, AgentStatus::Failed);
    assert!(report
        .result
        .failure
        .as_ref()
        .is_some_and(|failure| { failure.message.contains("Error while running the agent") }));
    assert_eq!(
        report.result.output_dir,
        temporary.path().join("output/contract")
    );
    assert_eq!(
        report.result.state_dir,
        temporary.path().join("state/contract")
    );
    assert!(report.result.output_dir.join("result.json").is_file());
    let journal = fs::read_to_string(report.result.output_dir.join("replay-events.jsonl")).unwrap();
    let terminal: Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
    assert_eq!(terminal["event"], "run_failed");
    assert!(report.result.artifacts.iter().any(|artifact| {
        artifact.role == "continued_native_trajectory" && artifact.path.is_file()
    }));
}
