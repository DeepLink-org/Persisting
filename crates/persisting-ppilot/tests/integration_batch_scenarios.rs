#![cfg(feature = "query")]

use persisting_control::RunState;
use persisting_ppilot::{
    process_trajectories, produce_from_planner, AnalysisOutputFormat, BatchAnalysisOptions,
    BatchProductionOptions,
};
use std::collections::BTreeSet;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn batch_analysis_uses_balanced_non_overlapping_chronicle_shards() {
    let temp = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let output = temp.path().join("analysis");
    let report = process_trajectories(BatchAnalysisOptions {
        input,
        sql: "SELECT session_id FROM runs ORDER BY session_id".into(),
        output_dir: output.clone(),
        parallelism: 3,
        format: AnalysisOutputFormat::Jsonl,
    })
    .await
    .unwrap();

    assert_eq!(report.trajectories, 8);
    assert_eq!(report.shard_count, 3);
    let sizes = report
        .shards
        .iter()
        .map(|shard| shard.trajectory_ids.len())
        .collect::<Vec<_>>();
    assert_eq!(sizes, [3, 3, 2]);
    let ids = report
        .shards
        .iter()
        .flat_map(|shard| shard.trajectory_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 8);
    assert!(report.shards.iter().all(|shard| shard.output.is_file()));
    assert!(report.output.is_file());
    assert!(output.join("analysis-report.json").is_file());
    let rows = std::fs::read_to_string(report.output).unwrap();
    assert_eq!(rows.lines().count(), 8);
}

#[test]
fn analysis_cli_runs_sharded_sql_and_writes_report() {
    let temp = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let output = temp.path().join("analysis-cli");
    let result = Command::new(env!("CARGO_BIN_EXE_ppilot"))
        .arg("analysis")
        .arg(&input)
        .args([
            "--output",
            output.to_str().unwrap(),
            "--parallelism",
            "3",
            "--fmt",
            "json",
        ])
        .args(["--sql", "SELECT session_id FROM runs ORDER BY session_id"])
        .output()
        .expect("run ppilot analysis");
    assert!(
        result.status.success(),
        "ppilot analysis failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    assert!(result.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Vec<serde_json::Value>>(
            &std::fs::read(output.join("results.json")).unwrap()
        )
        .unwrap()
        .len(),
        8
    );
    assert!(output.join("analysis-report.json").is_file());
}

#[test]
fn analysis_cli_defaults_to_stdout_and_supports_json_and_toml() {
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let run = |format: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ppilot"));
        command
            .arg("analysis")
            .arg(&input)
            .args(["--parallelism", "3"])
            .args(["--sql", "SELECT session_id FROM runs"]);
        if let Some(format) = format {
            command.args(["--fmt", format]);
        }
        command.output().expect("run terminal ppilot analysis")
    };

    let jsonl = run(None);
    assert!(jsonl.status.success());
    assert_eq!(String::from_utf8(jsonl.stdout).unwrap().lines().count(), 8);

    let json = run(Some("json"));
    assert!(json.status.success());
    assert_eq!(
        serde_json::from_slice::<Vec<serde_json::Value>>(&json.stdout)
            .unwrap()
            .len(),
        8
    );

    let toml = run(Some("toml"));
    assert!(toml.status.success());
    let document: toml::Value = toml::from_str(std::str::from_utf8(&toml.stdout).unwrap()).unwrap();
    assert_eq!(document["rows"].as_array().unwrap().len(), 8);
}
