//! Unique control-plane sink: ready results append here (not from Executors).
//!
//! **Primitive:** [`ResultSink`] · [`persist_terminal`] · [`JsonlFileSink`] (task_id dedup).
//!
//! - Phase-1 ledger: append-only JSONL under `--sink` (`task_id` for `--resume`).
//! - Optional L1: feature `traj-sink` → [`crate::sink_traj::LanceResultSink`] via Tee.

use crate::checkpoint::CheckpointLedger;
use crate::task::TaskResult;
use anyhow::{Context, Result};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

#[async_trait]
pub trait ResultSink: Send + Sync {
    /// Ready / terminal result that belongs in the asset ledger.
    async fn append_ready(&self, result: &TaskResult) -> Result<()>;
    /// Side ledger (infra / execute failure). Must not mix into training pool.
    async fn append_failure(&self, result: &TaskResult) -> Result<()>;
}

/// Append-only JSONL under `{root}/ready.ndjson` + `{root}/failures.ndjson`.
///
/// Idempotent by `task_id`: seeds from existing files on open, then skips duplicates.
/// `seen` is reserved before write and **rolled back** if the durable append fails,
/// so a failed write never permanently blocks a later retry of the same id.
pub struct JsonlFileSink {
    root: PathBuf,
    seen_ready: Mutex<HashSet<String>>,
    seen_fail: Mutex<HashSet<String>>,
}

impl JsonlFileSink {
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root)
            .await
            .with_context(|| format!("mkdir sink {}", root.display()))?;
        // Seed from disk so infra-retry / crash-resume duplicates do not re-append.
        let ledger = CheckpointLedger::load(&root).await?;
        Ok(Self {
            root,
            seen_ready: Mutex::new(ledger.ready),
            seen_fail: Mutex::new(ledger.failed),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn append_line(&self, file: &str, result: &TaskResult) -> Result<()> {
        let path = self.root.join(file);
        let line = result.to_ndjson()?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    /// Reserve `task_id` in `seen`. Returns `false` if already present (caller skips).
    fn reserve(seen: &Mutex<HashSet<String>>, task_id: &str) -> Result<bool> {
        let mut g = seen
            .lock()
            .map_err(|_| anyhow::anyhow!("sink lock poisoned"))?;
        Ok(g.insert(task_id.to_string()))
    }

    fn unreserve(seen: &Mutex<HashSet<String>>, task_id: &str) {
        if let Ok(mut g) = seen.lock() {
            g.remove(task_id);
        }
    }
}

#[async_trait]
impl ResultSink for JsonlFileSink {
    async fn append_ready(&self, result: &TaskResult) -> Result<()> {
        if !Self::reserve(&self.seen_ready, &result.task_id)? {
            return Ok(());
        }
        match self.append_line("ready.ndjson", result).await {
            Ok(()) => Ok(()),
            Err(e) => {
                Self::unreserve(&self.seen_ready, &result.task_id);
                Err(e)
            }
        }
    }

    async fn append_failure(&self, result: &TaskResult) -> Result<()> {
        if !Self::reserve(&self.seen_fail, &result.task_id)? {
            return Ok(());
        }
        match self.append_line("failures.ndjson", result).await {
            Ok(()) => Ok(()),
            Err(e) => {
                Self::unreserve(&self.seen_fail, &result.task_id);
                Err(e)
            }
        }
    }
}

/// Fan-out to several sinks (e.g. stdout view + durable JSONL).
pub struct TeeSink {
    sinks: Vec<Box<dyn ResultSink>>,
}

impl TeeSink {
    pub fn new(sinks: Vec<Box<dyn ResultSink>>) -> Self {
        Self { sinks }
    }
}

#[async_trait]
impl ResultSink for TeeSink {
    async fn append_ready(&self, result: &TaskResult) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        for s in &self.sinks {
            if let Err(e) = s.append_ready(result).await {
                tracing::error!(
                    task_id = %result.task_id,
                    error = %e,
                    "tee sink append_ready failed (continuing siblings)"
                );
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn append_failure(&self, result: &TaskResult) -> Result<()> {
        let mut first_err: Option<anyhow::Error> = None;
        for s in &self.sinks {
            if let Err(e) = s.append_failure(result).await {
                tracing::error!(
                    task_id = %result.task_id,
                    error = %e,
                    "tee sink append_failure failed (continuing siblings)"
                );
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Route by result status: cancelled/failed → failure ledger; ok → ready.
pub async fn persist_terminal(sink: &dyn ResultSink, result: &TaskResult) -> Result<()> {
    if result.ok && !result.cancelled {
        sink.append_ready(result).await
    } else {
        sink.append_failure(result).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn open_seeds_seen_and_skips_duplicate_append() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut ready = tokio::fs::File::create(root.join("ready.ndjson"))
            .await
            .unwrap();
        ready
            .write_all(br#"{"task_id":"t-0","ok":true}"#)
            .await
            .unwrap();
        ready.write_all(b"\n").await.unwrap();

        let sink = JsonlFileSink::open(root).await.unwrap();
        let dup = TaskResult::success("t-0", json!({"x": 1}), "w0", 0.0);
        sink.append_ready(&dup).await.unwrap();

        let body = tokio::fs::read_to_string(root.join("ready.ndjson"))
            .await
            .unwrap();
        assert_eq!(body.lines().count(), 1, "duplicate must not append: {body}");
    }

    #[tokio::test]
    async fn persist_terminal_routes_ok_and_fail() {
        let dir = tempfile::tempdir().unwrap();
        let sink = JsonlFileSink::open(dir.path()).await.unwrap();
        persist_terminal(&sink, &TaskResult::success("ok-1", json!(1), "w0", 0.0))
            .await
            .unwrap();
        persist_terminal(&sink, &TaskResult::cancelled("c-1"))
            .await
            .unwrap();
        persist_terminal(&sink, &TaskResult::failure("f-1", "e", None, "w0", 0.0))
            .await
            .unwrap();
        let ready = tokio::fs::read_to_string(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        let fail = tokio::fs::read_to_string(dir.path().join("failures.ndjson"))
            .await
            .unwrap();
        assert!(ready.contains("ok-1"));
        assert!(fail.contains("c-1") && fail.contains("f-1"));
        assert!(!ready.contains("c-1"));
    }

    #[tokio::test]
    async fn tee_fanout_to_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let a = JsonlFileSink::open(dir.path().join("a")).await.unwrap();
        let b = JsonlFileSink::open(dir.path().join("b")).await.unwrap();
        let tee = TeeSink::new(vec![Box::new(a), Box::new(b)]);
        persist_terminal(&tee, &TaskResult::success("t", json!(1), "w0", 0.0))
            .await
            .unwrap();
        for sub in ["a", "b"] {
            let body = tokio::fs::read_to_string(dir.path().join(sub).join("ready.ndjson"))
                .await
                .unwrap();
            assert!(body.contains("\"task_id\":\"t\""));
        }
    }

    #[tokio::test]
    async fn reopen_skips_duplicate_ready_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        {
            let sink = JsonlFileSink::open(dir.path()).await.unwrap();
            persist_terminal(&sink, &TaskResult::success("t-0", json!(1), "w0", 0.0))
                .await
                .unwrap();
        }
        let sink = JsonlFileSink::open(dir.path()).await.unwrap();
        persist_terminal(&sink, &TaskResult::success("t-0", json!(99), "w0", 0.0))
            .await
            .unwrap();
        let body = tokio::fs::read_to_string(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        assert_eq!(body.lines().count(), 1);
    }

    #[tokio::test]
    async fn write_failure_unreserves_seen_so_retry_can_persist() {
        // P0 regression: seen-before-write must not permanently drop a task_id.
        let dir = tempfile::tempdir().unwrap();
        let sink = JsonlFileSink::open(dir.path()).await.unwrap();
        // Make ready.ndjson a directory so OpenOptions::open fails.
        tokio::fs::create_dir(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        let r = TaskResult::success("t-stuck", json!(1), "w0", 0.0);
        assert!(
            sink.append_ready(&r).await.is_err(),
            "first append must fail"
        );
        tokio::fs::remove_dir(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        sink.append_ready(&r).await.expect("retry after fix path");
        let body = tokio::fs::read_to_string(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        assert_eq!(body.lines().count(), 1);
        assert!(body.contains("t-stuck"));
    }
}
