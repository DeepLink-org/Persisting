//! Helpers for crate integration tests (`tests/integration_*.rs`).

#![allow(dead_code)]

use persisting_ppilot::{Observer, RunOptions, SkipSet};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

pub fn write_plan(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write plan");
    path
}

pub fn run_opts(script: PathBuf) -> RunOptions {
    RunOptions {
        script,
        python: PathBuf::from("python3"),
        workers: 2,
        max_inflight: 4,
        per_worker_inflight: 1,
        pythonpath_extra: vec![],
        script_args: vec![],
        infra_retries: 0,
        job_cancel: CancellationToken::new(),
        observer: Observer::disabled(),
        skip_task_ids: SkipSet::new(),
        checkpoint: None,
        sink_submitter: None,
    }
}
