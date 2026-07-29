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
use persisting_ppilot::{TaskExpr, WorkerActor, WorkerCommand, WorkerReply};
use pulsing_actor::prelude::*;
use serde_json::json;
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
        WorkerCommand::Execute { task_json },
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
