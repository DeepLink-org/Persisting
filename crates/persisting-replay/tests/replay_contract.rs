use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            "mode": "replay_only"
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
    assert!(reconstructed.is_file());
    assert!(!continued.exists());
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
            "mode": "replay_and_continue"
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
    assert_eq!(fs::read_to_string(live_marker).unwrap().lines().count(), 2);
    assert!(reconstructed.is_file());
    assert!(continued.is_file());
}
