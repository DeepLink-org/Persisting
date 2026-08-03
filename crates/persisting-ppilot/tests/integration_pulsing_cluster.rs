//! Integration: two in-process Pulsing ActorSystems (seed + peer).
//!
//! Covers named resolve, job-control cancel, and **cross-node Execute**
//! without requiring torchrun.

mod common;

use persisting_ppilot::dist::DistEnv;
use persisting_ppilot::job_control::{
    broadcast_job_cancel, JobControlActor, JobControlCommand, JobControlReply,
};
use persisting_ppilot::pulsing_ext::{ask_timeout, resolve_actor, ASK_TIMEOUT};
use persisting_ppilot::{
    federated_count_with_workers, process_script_with_workers, CountTable,
    FederatedAnalysisCommand, FederatedAnalysisReply, FederatedAnalysisWorker,
    FederatedCountOptions, FederatedCountReport, ProcessScriptOptions,
};
use persisting_ppilot::{TaskExpr, WorkerActor, WorkerCommand, WorkerReply};
use pulsing_actor::prelude::*;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

async fn join_peer(seed_addr: &str) -> Arc<ActorSystem> {
    let mut last = None;
    for _ in 1..=40 {
        match ActorSystem::builder()
            .mailbox_capacity(64)
            .addr("127.0.0.1:0")
            .seeds([seed_addr])
            .build()
            .await
        {
            Ok(s) => return s,
            Err(e) => {
                last = Some(e.to_string());
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    panic!("peer join failed: {last:?}");
}

async fn resolve_eventually(system: &ActorSystem, name: &str) -> ActorRef {
    for _ in 0..80 {
        if let Ok(r) = resolve_actor(system, name).await {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out resolving {name}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_systems_resolve_and_cancel() {
    let seed: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr("127.0.0.1:0")
        .build()
        .await
        .expect("seed system");
    let seed_addr = seed.addr().to_string();

    let t0 = CancellationToken::new();
    seed.spawn_named(
        DistEnv::job_control_name(0),
        JobControlActor::new(t0.clone()),
    )
    .await
    .unwrap();

    let peer = join_peer(&seed_addr).await;

    let t1 = CancellationToken::new();
    peer.spawn_named(
        DistEnv::job_control_name(1),
        JobControlActor::new(t1.clone()),
    )
    .await
    .unwrap();

    let remote_ctrl = resolve_eventually(seed.as_ref(), &DistEnv::job_control_name(1)).await;

    let ack =
        ask_timeout::<_, JobControlReply>(&remote_ctrl, JobControlCommand::Cancel, ASK_TIMEOUT)
            .await
            .expect("ask cancel on remote");
    assert!(matches!(ack, JobControlReply::Ack { already: false }));
    assert!(t1.is_cancelled());

    broadcast_job_cancel(&seed, 2).await;
    assert!(t0.is_cancelled());

    peer.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
}

/// Cross-node Execute: peer hosts a WorkerActor; seed resolves + ask.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_systems_remote_execute() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "remote_exec.py",
        r#"
def plan():
    yield {"id": "unused"}

def execute(item):
    return {"echo": item.get("x", 0), "from": "peer"}
"#,
    );

    let seed: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr("127.0.0.1:0")
        .build()
        .await
        .expect("seed system");
    let seed_addr = seed.addr().to_string();

    let peer = join_peer(&seed_addr).await;
    let job_cancel = CancellationToken::new();
    let worker_name = DistEnv::slot_name(1, 0, 1);
    peer.spawn_named(
        &worker_name,
        WorkerActor::with_plan(
            "w1",
            PathBuf::from("python3"),
            vec![],
            script,
            vec![],
            job_cancel,
        ),
    )
    .await
    .unwrap();

    let remote = resolve_eventually(seed.as_ref(), &worker_name).await;
    let task = TaskExpr::from_value(json!({"id": "t-remote", "x": 42})).unwrap();
    let task_json = serde_json::to_vec(&task).unwrap();
    let reply = ask_timeout::<_, WorkerReply>(
        &remote,
        WorkerCommand::Execute {
            task_json,
            lease_epoch: 1,
        },
        Duration::from_secs(30),
    )
    .await
    .expect("remote Execute");

    let WorkerReply::Result { result_json } = reply else {
        panic!("unexpected reply: {reply:?}");
    };
    let result: persisting_ppilot::TaskResult = serde_json::from_slice(&result_json).unwrap();
    assert!(result.ok, "remote execute failed: {:?}", result.error);
    assert_eq!(result.task_id, "t-remote");
    assert_eq!(
        result.value.as_ref().and_then(|v| v.get("echo")),
        Some(&json!(42))
    );

    peer.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
}

/// Run one analysis metric over two independently listening Pulsing systems.
/// The helper also checks the invariants shared by all five production cases:
/// exact-once trajectory coverage, deterministic shard ids, both worker ranks,
/// durable partials and a coordinator-owned final result.
async fn run_federated_analysis_case(
    table: CountTable,
    expected: u64,
    parallelism: usize,
) -> FederatedCountReport {
    let output = tempfile::tempdir().unwrap();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../persisting-pchronicle/tests/fixtures/atif");
    let seed: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr("127.0.0.1:0")
        .build()
        .await
        .expect("seed system");
    let seed_addr = seed.addr().to_string();
    let worker0 = seed
        .spawn_named(
            DistEnv::analysis_worker_name(0),
            FederatedAnalysisWorker::new(0, CancellationToken::new()),
        )
        .await
        .unwrap();

    let peer = join_peer(&seed_addr).await;
    peer.spawn_named(
        DistEnv::analysis_worker_name(1),
        FederatedAnalysisWorker::new(1, CancellationToken::new()),
    )
    .await
    .unwrap();
    let worker1 = resolve_eventually(seed.as_ref(), &DistEnv::analysis_worker_name(1)).await;

    let report = federated_count_with_workers(
        FederatedCountOptions {
            input,
            output_dir: output.path().join("analysis"),
            parallelism,
            table,
        },
        vec![worker0.clone(), worker1.clone()],
    )
    .await
    .unwrap();

    assert_eq!(report.aggregation_levels, 2);
    assert_eq!(report.worker_count, 2);
    assert_eq!(report.shard_count, parallelism.clamp(1, 8));
    assert_eq!(report.trajectories, 8);
    assert_eq!(report.table, table);
    assert_eq!(report.count, expected);
    assert_eq!(
        report
            .partials
            .iter()
            .map(|partial| partial.count)
            .sum::<u64>(),
        report.count
    );
    assert_eq!(
        report
            .partials
            .iter()
            .map(|partial| partial.worker_rank)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([0, 1])
    );
    let trajectory_ids = report
        .partials
        .iter()
        .flat_map(|partial| partial.trajectory_ids.iter().cloned())
        .collect::<Vec<_>>();
    assert_eq!(trajectory_ids.len(), 8);
    assert_eq!(trajectory_ids.iter().collect::<BTreeSet<_>>().len(), 8);
    assert!(report
        .partials
        .iter()
        .all(|partial| !partial.trajectory_ids.is_empty()));
    assert!(report.output.is_file());
    assert!(output
        .path()
        .join("analysis/analysis-report.json")
        .is_file());
    let final_row: serde_json::Value =
        serde_json::from_str(std::fs::read_to_string(&report.output).unwrap().trim()).unwrap();
    assert_eq!(final_row["table"], table.as_str());
    assert_eq!(final_row["count"], expected);
    assert!(report
        .partials
        .iter()
        .all(|partial| partial.output.is_file() && partial.payload_bytes > 0));

    for worker in [&worker0, &worker1] {
        let reply = ask_timeout::<_, FederatedAnalysisReply>(
            worker,
            FederatedAnalysisCommand::Shutdown,
            ASK_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(reply, FederatedAnalysisReply::Stopped));
    }
    peer.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
    report
}

/// Case 1: corpus/run inventory. Parallelism exceeds the trajectory count,
/// proving the coordinator does not create empty shards or double-count Runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_case_1_trajectory_inventory() {
    let report = run_federated_analysis_case(CountTable::Runs, 8, 16).await;
    assert_eq!(report.shard_count, 8);
    assert!(report
        .partials
        .iter()
        .all(|partial| partial.trajectory_ids.len() == 1 && partial.count == 1));
}

/// Case 2: interaction workload. Uneven trajectory lengths produce unequal
/// partials, while the two-level reduction must retain the exact step total.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_case_2_interaction_step_volume() {
    let report = run_federated_analysis_case(CountTable::Steps, 118, 4).await;
    let counts = report
        .partials
        .iter()
        .map(|partial| partial.count)
        .collect::<BTreeSet<_>>();
    assert!(
        counts.len() > 1,
        "fixture should exercise skewed shard loads"
    );
}

/// Case 3: tool adoption. One trajectory has no tool calls, so the reducer
/// must preserve a legitimate zero-valued partial instead of treating it as
/// a missing worker response.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_case_3_sparse_tool_usage() {
    let report = run_federated_analysis_case(CountTable::ToolCalls, 23, 8).await;
    assert!(report.partials.iter().any(|partial| partial.count == 0));
    assert!(report.partials.iter().any(|partial| partial.count > 1));
}

/// Case 4: model cost. `llm_call_count` may be greater than one on a single
/// step, so this exercises a distributive SUM rather than physical row count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_case_4_llm_call_cost() {
    let report = run_federated_analysis_case(CountTable::LlmCalls, 52, 5).await;
    assert!(report.partials.iter().all(|partial| partial.count > 0));
}

/// Case 5: context efficiency. This is a filtered distributed count and
/// includes a valid zero-valued shard for a trajectory with no copied context.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn federated_case_5_copied_context_overhead() {
    let report = run_federated_analysis_case(CountTable::CopiedContextSteps, 19, 8).await;
    assert!(report.partials.iter().any(|partial| partial.count == 0));
    assert!(report.partials.iter().any(|partial| partial.count >= 4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_systems_run_transferred_python_map_reduce_script() {
    let temporary = tempfile::tempdir().unwrap();
    let script = temporary.path().join("agent_metrics.py");
    std::fs::write(
        &script,
        r#"
def mapper(records, context):
    print(f"mapped shard {context['shard_id']}")
    return {
        "runs": len(records),
        "steps": sum(len(record["steps"]) for record in records),
        "worker_rank": context["worker_rank"],
    }

def reducer(partials):
    return {
        "runs": sum(partial["runs"] for partial in partials),
        "steps": sum(partial["steps"] for partial in partials),
        "worker_ranks": sorted(set(partial["worker_rank"] for partial in partials)),
    }
"#,
    )
    .unwrap();
    let seed: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(64)
        .addr("127.0.0.1:0")
        .build()
        .await
        .unwrap();
    let worker0 = seed
        .spawn_named(
            DistEnv::analysis_worker_name(0),
            FederatedAnalysisWorker::new(0, CancellationToken::new()),
        )
        .await
        .unwrap();
    let peer = join_peer(&seed.addr().to_string()).await;
    peer.spawn_named(
        DistEnv::analysis_worker_name(1),
        FederatedAnalysisWorker::new(1, CancellationToken::new()),
    )
    .await
    .unwrap();
    let worker1 = resolve_eventually(seed.as_ref(), &DistEnv::analysis_worker_name(1)).await;

    let output = temporary.path().join("processed");
    let report = process_script_with_workers(
        ProcessScriptOptions {
            input: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../persisting-pchronicle/tests/fixtures/atif"),
            script,
            output_dir: Some(output.clone()),
            mappers: 4,
            python: PathBuf::from("python3"),
        },
        vec![worker0.clone(), worker1.clone()],
    )
    .await
    .unwrap();
    assert_eq!(report.mode, "python_map_reduce");
    assert_eq!(report.shard_count, 4);
    assert_eq!(report.trajectories, 8);
    assert_eq!(report.result["runs"], 8);
    assert_eq!(report.result["steps"], 118);
    assert_eq!(report.result["worker_ranks"], json!([0, 1]));
    assert_eq!(report.script_sha256.len(), 64);
    assert!(output.join("results.json").is_file());
    assert!(output.join("process-report.json").is_file());

    for worker in [&worker0, &worker1] {
        let reply = ask_timeout::<_, FederatedAnalysisReply>(
            worker,
            FederatedAnalysisCommand::Shutdown,
            ASK_TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(reply, FederatedAnalysisReply::Stopped));
    }
    peer.shutdown().await.unwrap();
    seed.shutdown().await.unwrap();
}
