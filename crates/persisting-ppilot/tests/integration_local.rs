//! Integration: local fleet paths that cross Driver + Runtime + Worker + L3.
//!
//! Contract unit tests live next to their modules (`src/*.rs`). This file only
//! covers multi-module behavior.

mod common;

use persisting_ppilot::{
    run_local_fleet, spawn_coordinated_sink_writer, spawn_sink_writer, CheckpointLedger,
    JsonlFileSink, Observer, RunCoordinator, RunOptions, SkipSet,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completion_order_across_fleet() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "delay.py",
        r#"
import time

def plan():
    for i in range(4):
        yield {"id": f"t-{i}", "delay": 0.3 if i == 0 else 0.01}

def execute(item):
    time.sleep(float(item["delay"]))
    return {"x": item["id"]}
"#,
    );
    let order = Arc::new(Mutex::new(Vec::new()));
    let order_cb = Arc::clone(&order);
    let mut opts = common::run_opts(script);
    opts.workers = 2;
    opts.max_inflight = 4;
    run_local_fleet(opts, move |r| {
        order_cb.lock().unwrap().push(r.task_id.clone());
    })
    .await
    .unwrap();
    let seen = order.lock().unwrap().clone();
    assert_ne!(
        seen[0], "t-0",
        "expected fast task before slow t-0: {seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn driver_worker_pvisor_result_is_fenced_and_committed() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "committed.py",
        r#"
def plan():
    yield {"id": "committed-task", "value": 7}

def execute(item):
    return {"value": item["value"]}
"#,
    );
    let sink_root = dir.path().join("sink");
    let sink: Arc<dyn persisting_ppilot::ResultSink> =
        Arc::new(JsonlFileSink::open(&sink_root).await.unwrap());
    let coordinator = Arc::new(
        RunCoordinator::open(dir.path().to_string_lossy(), &sink_root, 30_000)
            .await
            .unwrap(),
    );
    let writer = spawn_coordinated_sink_writer(sink, None, None, 8, Arc::clone(&coordinator));
    let mut opts = common::run_opts(script);
    opts.workers = 1;
    opts.max_inflight = 1;
    opts.sink_submitter = Some(writer.submitter());
    opts.coordinator = Some(Arc::clone(&coordinator));

    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    writer.join().await.unwrap();
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert!(result.ok, "unexpected pVisor result: {result:#?}");
    assert!(result.lease_epoch > 0);
    assert!(result.attempt_id.is_some());
    let run_id = persisting_control::RunId::new(result.run_id.as_deref().unwrap());
    let control = coordinator.control().get(&run_id).await.unwrap().unwrap();
    let commit = control.commit.expect("terminal RunCommit");
    assert_eq!(commit.request.lease_epoch, result.lease_epoch);
    assert_eq!(
        commit.request.attempt_id.as_str(),
        result.attempt_id.as_deref().unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_skip_ids_not_dispatched() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "skip.py",
        r#"
def plan():
    for i in range(3):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return {"x": item["x"]}
"#,
    );
    let mut opts = common::run_opts(script);
    opts.workers = 1;
    opts.skip_task_ids = ["t-0".into(), "t-1".into()].into_iter().collect();
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].task_id, "t-2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ctrl_c_cancels_in_flight_execute() {
    let dir = tempfile::tempdir().unwrap();
    let started = dir.path().join("execute-started");
    let started_literal = serde_json::to_string(started.to_str().unwrap()).unwrap();
    let script = common::write_plan(
        dir.path(),
        "slow.py",
        &format!(
            r#"
import time
from pathlib import Path

def plan():
    yield {{"id": "slow"}}

def execute(item):
    Path({started_literal}).write_text("started")
    time.sleep(5)
    return {{}}
"#
        ),
    );
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !tokio::fs::try_exists(&started).await.unwrap_or(false) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "execute did not start before cancellation deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        c.cancel();
    });
    let opts = RunOptions {
        script,
        python: PathBuf::from("python3"),
        workers: 1,
        max_inflight: 1,
        per_worker_inflight: 1,
        pythonpath_extra: vec![],
        script_args: vec![],
        infra_retries: 0,
        job_cancel: cancel,
        observer: Observer::disabled(),
        skip_task_ids: SkipSet::new(),
        checkpoint: None,
        sink_submitter: None,
        coordinator: None,
    };
    let t0 = Instant::now();
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert!(results.iter().any(|r| r.cancelled));
    assert!(t0.elapsed().as_secs_f64() < 2.5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_worker_slots_run_in_parallel() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = dir.path().join("parallel-barrier");
    std::fs::create_dir(&barrier).unwrap();
    let barrier_literal = serde_json::to_string(barrier.to_str().unwrap()).unwrap();
    let script = common::write_plan(
        dir.path(),
        "par.py",
        &format!(
            r#"
import time
from pathlib import Path

BARRIER = Path({barrier_literal})

def plan():
    for i in range(2):
        yield {{"id": f"t-{{i}}"}}

def execute(item):
    (BARRIER / f"ready-{{item['id']}}").write_text("ready")
    deadline = time.monotonic() + 5
    while len(list(BARRIER.glob("ready-*"))) < 2:
        if time.monotonic() >= deadline:
            return {{"overlapped": False}}
        time.sleep(0.01)
    return {{"overlapped": True}}
"#,
        ),
    );
    let mut opts = common::run_opts(script);
    opts.workers = 1;
    opts.per_worker_inflight = 2;
    opts.max_inflight = 2;
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.ok));
    assert!(
        results.iter().all(|r| r
            .value
            .as_ref()
            .is_some_and(|value| value["overlapped"] == true)),
        "worker slots did not overlap: {results:?}"
    );
    let workers: HashSet<_> = results.iter().filter_map(|r| r.worker.clone()).collect();
    assert_eq!(workers.len(), 2, "expected two slot ids, got {workers:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn script_args_reach_plan_and_execute() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "args.py",
        r#"
import argparse

def _parse(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("--n", type=int, default=1)
    return p.parse_args(argv)

def plan():
    args = _parse()
    for i in range(args.n):
        yield {"id": f"t-{i}", "n": args.n}

def execute(item):
    return {"n": item["n"]}
"#,
    );
    let mut opts = common::run_opts(script);
    opts.workers = 1;
    opts.script_args = vec!["--n".into(), "3".into()];
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].value.as_ref().unwrap()["n"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_plan_ids_dispatch_once() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "dup.py",
        r#"
def plan():
    yield {"id": "same", "x": 1}
    yield {"id": "same", "x": 2}

def execute(item):
    return {"x": item["x"]}
"#,
    );
    let mut opts = common::run_opts(script);
    opts.workers = 2;
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert_eq!(
        results.len(),
        1,
        "second identical id must be claimed/skipped"
    );
    assert_eq!(results[0].task_id, "same");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_inflight_handles_many_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let script = common::write_plan(
        dir.path(),
        "many.py",
        r#"
def plan():
    for i in range(40):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return {"x": item["x"] * 2}
"#,
    );
    let mut opts = common::run_opts(script);
    opts.workers = 2;
    opts.max_inflight = 2;
    opts.per_worker_inflight = 1;
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    assert_eq!(results.len(), 40);
    assert!(results.iter().all(|r| r.ok));
}

/// `--sink` persist then `--resume` from ledger, including corrupt JSONL lines.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sink_then_resume_skips_done_and_tolerates_bad_lines() {
    let dir = tempfile::tempdir().unwrap();
    let sink_root = dir.path().join("sink");
    let script = common::write_plan(
        dir.path(),
        "resume.py",
        r#"
def plan():
    for i in range(3):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return {"x": item["x"]}
"#,
    );

    // Run 1: all three tasks → ready.ndjson
    {
        let sink: Arc<dyn persisting_ppilot::ResultSink> =
            Arc::new(JsonlFileSink::open(&sink_root).await.unwrap());
        let writer = spawn_sink_writer(sink, None, None, 32);
        let mut opts = common::run_opts(script.clone());
        opts.workers = 1;
        opts.sink_submitter = Some(writer.submitter());
        let results = run_local_fleet(opts, |_| {}).await.unwrap();
        writer.join().await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.ok));
    }

    // Corrupt a line; valid terminals must still load for resume.
    {
        let ready_path = sink_root.join("ready.ndjson");
        let mut body = std::fs::read_to_string(&ready_path).unwrap();
        body.push_str("this-is-not-json\n");
        body.push_str("{\"no_id\":true}\n");
        std::fs::write(&ready_path, body).unwrap();
    }

    let ledger = CheckpointLedger::load(&sink_root).await.unwrap();
    assert_eq!(
        ledger.ready.len(),
        3,
        "corrupt lines must not drop valid ids"
    );
    assert!(ledger.skip_ids().contains("t-0"));
    assert!(ledger.skip_ids().contains("t-1"));
    assert!(ledger.skip_ids().contains("t-2"));

    // Run 2: resume — nothing left to dispatch.
    {
        let sink: Arc<dyn persisting_ppilot::ResultSink> =
            Arc::new(JsonlFileSink::open(&sink_root).await.unwrap());
        let writer = spawn_sink_writer(sink, None, None, 32);
        let mut opts = common::run_opts(script.clone());
        opts.workers = 1;
        opts.skip_task_ids = ledger.skip_ids().into_iter().collect();
        opts.sink_submitter = Some(writer.submitter());
        let results = run_local_fleet(opts, |_| {}).await.unwrap();
        writer.join().await.unwrap();
        assert!(
            results.is_empty(),
            "resume must skip all ledger terminals, got {results:?}"
        );
    }

    // Partial resume: only t-0 done on disk → run t-1/t-2.
    let partial = dir.path().join("sink_partial");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::write(
        partial.join("ready.ndjson"),
        "{\"task_id\":\"t-0\",\"ok\":true}\nnot-json\n",
    )
    .unwrap();
    let partial_ledger = CheckpointLedger::load(&partial).await.unwrap();
    assert_eq!(partial_ledger.ready, HashSet::from(["t-0".into()]));

    let sink: Arc<dyn persisting_ppilot::ResultSink> =
        Arc::new(JsonlFileSink::open(&partial).await.unwrap());
    let writer = spawn_sink_writer(sink, None, None, 32);
    let mut opts = common::run_opts(script);
    opts.workers = 1;
    opts.skip_task_ids = partial_ledger.skip_ids().into_iter().collect();
    opts.sink_submitter = Some(writer.submitter());
    let results = run_local_fleet(opts, |_| {}).await.unwrap();
    writer.join().await.unwrap();
    let ids: HashSet<_> = results.iter().map(|r| r.task_id.clone()).collect();
    assert_eq!(ids, HashSet::from(["t-1".into(), "t-2".into()]));
    assert!(results.iter().all(|r| r.ok));

    let after = CheckpointLedger::load(&partial).await.unwrap();
    assert_eq!(after.ready.len(), 3);
    assert!(after.skip_ids().contains("t-0"));
    assert!(after.skip_ids().contains("t-1"));
    assert!(after.skip_ids().contains("t-2"));
}
