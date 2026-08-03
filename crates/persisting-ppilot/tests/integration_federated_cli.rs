#![cfg(feature = "query")]

use serde_json::Value;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn distributed_process_command(
    rank: usize,
    pulsing_port: u16,
    input: &std::path::Path,
    output: &std::path::Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ppilot"));
    command
        .args([
            "process",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--parallelism",
            "4",
            "--count",
            "steps",
        ])
        .env("RANK", rank.to_string())
        .env("WORLD_SIZE", "2")
        .env("MASTER_ADDR", "127.0.0.1")
        .env("MASTER_PORT", "29500")
        .env("PERSISTING_PULSING_PORT", pulsing_port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_process_cli_runs_cross_rank_count_and_only_driver_writes_files() {
    let temp = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let output = temp.path().join("analysis");
    let pulsing_port = free_port();

    let driver = distributed_process_command(0, pulsing_port, &input, &output)
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    // Rank 1 deliberately receives a nonexistent input: only rank 0 owns
    // discovery and sends serialized shards over Pulsing.
    let missing_peer_input = temp.path().join("not-mounted-on-peer");
    let peer = distributed_process_command(1, pulsing_port, &missing_peer_input, &output)
        .spawn()
        .unwrap();

    let (driver_output, peer_output) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(driver.wait_with_output(), peer.wait_with_output())
    })
    .await
    .expect("distributed ppilot processes timed out");
    let driver_output = driver_output.unwrap();
    let peer_output = peer_output.unwrap();
    assert!(
        driver_output.status.success(),
        "driver stderr: {}",
        String::from_utf8_lossy(&driver_output.stderr)
    );
    assert!(
        peer_output.status.success(),
        "peer stderr: {}",
        String::from_utf8_lossy(&peer_output.stderr)
    );
    assert!(driver_output.stdout.is_empty());
    assert!(peer_output.stdout.is_empty());

    let report_bytes = std::fs::read(output.join("analysis-report.json")).unwrap();
    let report: Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(report["worker_count"], 2);
    assert_eq!(report["shard_count"], 4);
    assert_eq!(report["count"], 118);
    assert_eq!(
        std::fs::read_to_string(output.join("results.jsonl")).unwrap(),
        "{\"count\":118,\"table\":\"steps\"}\n"
    );
}

fn distributed_script_command(
    rank: usize,
    pulsing_port: u16,
    input: &std::path::Path,
    script: &std::path::Path,
    output: &std::path::Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ppilot"));
    command
        .arg("process")
        .arg(input)
        .args(["--script", script.to_str().unwrap(), "--mappers", "4"])
        .arg("--output")
        .arg(output)
        .env("RANK", rank.to_string())
        .env("WORLD_SIZE", "2")
        .env("MASTER_ADDR", "127.0.0.1")
        .env("MASTER_PORT", "29500")
        .env("PERSISTING_PULSING_PORT", pulsing_port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_process_cli_transfers_script_to_remote_mapper_and_reduces() {
    let temp = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let script = temp.path().join("job.py");
    std::fs::write(
        &script,
        r#"
def map(records, context):
    return {"runs": len(records), "worker": context["worker_rank"]}

def reduce(partials, context):
    return {
        "runs": sum(partial["runs"] for partial in partials),
        "workers": sorted(set(partial["worker"] for partial in partials)),
        "mappers": context["mapper_count"],
    }
"#,
    )
    .unwrap();
    let output = temp.path().join("processed");
    let pulsing_port = free_port();
    let driver = distributed_script_command(0, pulsing_port, &input, &script, &output)
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let peer = distributed_script_command(
        1,
        pulsing_port,
        &temp.path().join("peer-has-no-input"),
        &temp.path().join("peer-has-no-script.py"),
        &output,
    )
    .spawn()
    .unwrap();

    let (driver_output, peer_output) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(driver.wait_with_output(), peer.wait_with_output())
    })
    .await
    .expect("distributed script process timed out");
    let driver_output = driver_output.unwrap();
    let peer_output = peer_output.unwrap();
    assert!(
        driver_output.status.success(),
        "driver stderr: {}",
        String::from_utf8_lossy(&driver_output.stderr)
    );
    assert!(
        peer_output.status.success(),
        "peer stderr: {}",
        String::from_utf8_lossy(&peer_output.stderr)
    );
    assert!(driver_output.stdout.is_empty());
    assert!(peer_output.stdout.is_empty());

    let result: Value =
        serde_json::from_slice(&std::fs::read(output.join("results.json")).unwrap()).unwrap();
    assert_eq!(result["runs"], 8);
    assert_eq!(result["workers"], serde_json::json!([0, 1]));
    assert_eq!(result["mappers"], 4);
    let report: Value =
        serde_json::from_slice(&std::fs::read(output.join("process-report.json")).unwrap())
            .unwrap();
    assert_eq!(report["script_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(report["shard_count"], 4);
}
