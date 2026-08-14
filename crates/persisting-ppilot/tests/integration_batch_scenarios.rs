use persisting_agentctl::RunState;
use persisting_ppilot::{produce_from_planner, BatchProductionOptions};
use std::path::PathBuf;
use std::process::Command;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_production_runs_multiple_pvisors_with_reviewable_lineage() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("production");
    let planner = temp.path().join("production.py");
    std::fs::write(
        &planner,
        r#"
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, required=True)
args = parser.parse_args()

def plan():
    for index in range(args.count):
        yield {
            "id": f"trajectory-{index}",
            "agent": "fixture-agent",
            "command": ["/bin/sh", "-c", f"printf trajectory-{index}"],
        }
"#,
    )
    .unwrap();

    let report = produce_from_planner(
        planner,
        PathBuf::from("python3"),
        vec!["--count".into(), "3".into()],
        "integration".into(),
        BatchProductionOptions {
            output_dir: output.clone(),
            parallelism: 2,
            capture_gateway: true,
            supervisor_network_limit_bytes_per_second: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(report.total, 3);
    assert_eq!(report.completed, 3);
    assert_eq!(report.failed, 0);
    assert!(output.join("production-report.json").is_file());
    for outcome in report.runs {
        assert_eq!(outcome.state, RunState::Completed);
        let bundle = persisting_pvisor::RunBundle::read(&outcome.workspace).unwrap();
        assert_eq!(
            bundle
                .orchestration
                .get("persisting.ppilot.supervisor.connected"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            bundle.run.task_id.as_deref(),
            Some(outcome.task_id.as_str())
        );
        assert_eq!(
            bundle.run.parent_run_id.as_deref(),
            Some("ppilot-batch-integration")
        );
        assert_eq!(bundle.orchestration["ppilot.batch_id"], "integration");
        assert_eq!(
            bundle.orchestration["ppilot.scope"],
            "trajectory-production"
        );
        assert!(outcome.workspace.join("run.json").is_file());
        assert!(outcome.workspace.join("run-bundle.json").is_file());
    }
}

#[test]
fn produce_cli_uses_python_planner_as_primary_input() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("production-cli");
    let planner = temp.path().join("cli-production.py");
    std::fs::write(
        &planner,
        r#"
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, required=True)
args = parser.parse_args()

def plan():
    for index in range(args.count):
        yield {
            "id": f"cli-{index}",
            "agent": "fixture-agent",
            "command": ["/bin/sh", "-c", f"printf cli-{index}"],
        }
"#,
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_ppilot"))
        .arg("produce")
        .arg(&planner)
        .args(["--output", output.to_str().unwrap()])
        .args(["--parallelism", "2", "--batch-id", "cli"])
        .args(["--", "--count", "2"])
        .output()
        .expect("run ppilot produce");
    assert!(
        result.status.success(),
        "ppilot produce failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["batch_id"], "cli");
    assert_eq!(report["completed"], 2);
    assert!(output.join("production-report.json").is_file());
    assert!(output.join("cli-0/run-bundle.json").is_file());
    assert!(output.join("cli-1/run-bundle.json").is_file());
}
