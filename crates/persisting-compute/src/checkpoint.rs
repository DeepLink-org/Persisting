//! Checkpoint ledger: resume unfinished work from `--sink` + progress file.
//!
//! Done set = `task_id`s already in `ready.ndjson` / `failures.ndjson`.
//! Progress snapshot = `checkpoint.json` (throttled writes).

use crate::task::unix_now;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Terminal task ids already recorded under a sink root.
#[derive(Debug, Clone, Default)]
pub struct CheckpointLedger {
    pub ready: HashSet<String>,
    pub failed: HashSet<String>,
}

impl CheckpointLedger {
    pub fn skip_ids(&self) -> HashSet<String> {
        let mut s = self.ready.clone();
        s.extend(self.failed.iter().cloned());
        s
    }

    pub async fn load(root: &Path) -> Result<Self> {
        let ready = load_task_ids(&root.join("ready.ndjson")).await?;
        let failed = load_task_ids(&root.join("failures.ndjson")).await?;
        Ok(Self { ready, failed })
    }
}

async fn load_task_ids(path: &Path) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    if !path.exists() {
        return Ok(out);
    }
    let f = fs::File::open(path)
        .await
        .with_context(|| format!("open {}", path.display()))?;
    let mut lines = BufReader::new(f).lines();
    let mut line_no = 0u64;
    while let Some(line) = lines.next_line().await? {
        line_no += 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    line = line_no,
                    error = %e,
                    "skipping corrupt checkpoint JSONL line"
                );
                continue;
            }
        };
        if let Some(id) = v
            .get("task_id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
        {
            out.insert(id);
        } else {
            tracing::warn!(
                path = %path.display(),
                line = line_no,
                "skipping checkpoint line without task_id"
            );
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointProgress {
    pub ok: u64,
    pub fail: u64,
    pub cancelled: u64,
    pub skipped: u64,
    pub dispatched: u64,
    pub updated_at: f64,
}

/// Live progress tracker with throttled `checkpoint.json` writes.
pub struct CheckpointTracker {
    root: PathBuf,
    state: Mutex<CheckpointProgress>,
    last_flush: Mutex<Instant>,
    flush_every_ms: u64,
    dirty: AtomicU64,
}

impl CheckpointTracker {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state: Mutex::new(CheckpointProgress::default()),
            last_flush: Mutex::new(Instant::now() - std::time::Duration::from_secs(2)),
            flush_every_ms: 1000,
            dirty: AtomicU64::new(0),
        }
    }

    pub fn seed_from_ledger(&self, ledger: &CheckpointLedger) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.ok = ledger.ready.len() as u64;
        g.fail = ledger.failed.len() as u64;
        g.updated_at = unix_now();
    }

    pub fn note_skipped(&self, n: u64) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.skipped = g.skipped.saturating_add(n);
        g.updated_at = unix_now();
        self.dirty.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_dispatched(&self) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        g.dispatched = g.dispatched.saturating_add(1);
        g.updated_at = unix_now();
        self.dirty.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_terminal(&self, ok: bool, cancelled: bool) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if cancelled {
            g.cancelled = g.cancelled.saturating_add(1);
        } else if ok {
            g.ok = g.ok.saturating_add(1);
        } else {
            g.fail = g.fail.saturating_add(1);
        }
        g.updated_at = unix_now();
        self.dirty.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> CheckpointProgress {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn maybe_flush(&self) -> Result<()> {
        if self.dirty.load(Ordering::Relaxed) == 0 {
            return Ok(());
        }
        let should = {
            let last = self.last_flush.lock().unwrap_or_else(|e| e.into_inner());
            last.elapsed().as_millis() as u64 >= self.flush_every_ms
        };
        if should {
            self.flush().await?;
        }
        Ok(())
    }

    pub async fn flush(&self) -> Result<()> {
        let snap = self.snapshot();
        let path = self.root.join("checkpoint.json");
        let tmp = self.root.join("checkpoint.json.tmp");
        let body = serde_json::to_vec_pretty(&snap)?;
        fs::write(&tmp, &body)
            .await
            .with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .await
            .with_context(|| format!("rename {}", path.display()))?;
        if let Ok(mut last) = self.last_flush.lock() {
            *last = Instant::now();
        }
        self.dirty.store(0, Ordering::Relaxed);
        Ok(())
    }

    pub fn summary_line(&self) -> String {
        let s = self.snapshot();
        format!(
            "[ckpt] ok={} fail={} cancelled={} skipped={} dispatched={}",
            s.ok, s.fail, s.cancelled, s.skipped, s.dispatched
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn load_ready_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut ready = fs::File::create(root.join("ready.ndjson")).await.unwrap();
        ready
            .write_all(br#"{"task_id":"t-0","ok":true}"#)
            .await
            .unwrap();
        ready.write_all(b"\n").await.unwrap();
        let mut fail = fs::File::create(root.join("failures.ndjson"))
            .await
            .unwrap();
        fail.write_all(br#"{"task_id":"t-1","ok":false}"#)
            .await
            .unwrap();
        fail.write_all(b"\n").await.unwrap();

        let ledger = CheckpointLedger::load(root).await.unwrap();
        assert!(ledger.ready.contains("t-0"));
        assert!(ledger.failed.contains("t-1"));
        assert_eq!(ledger.skip_ids().len(), 2);
    }

    #[tokio::test]
    async fn tracker_flush_writes_checkpoint_json() {
        let dir = tempfile::tempdir().unwrap();
        let tracker = CheckpointTracker::new(dir.path());
        tracker.note_dispatched();
        tracker.note_terminal(true, false);
        tracker.note_skipped(2);
        tracker.flush().await.unwrap();
        let body = fs::read_to_string(dir.path().join("checkpoint.json"))
            .await
            .unwrap();
        let snap: CheckpointProgress = serde_json::from_str(&body).unwrap();
        assert_eq!(snap.ok, 1);
        assert_eq!(snap.dispatched, 1);
        assert_eq!(snap.skipped, 2);
    }

    #[tokio::test]
    async fn empty_dir_loads_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = CheckpointLedger::load(dir.path()).await.unwrap();
        assert!(ledger.skip_ids().is_empty());
    }

    #[tokio::test]
    async fn skip_ids_is_ready_union_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut ready = fs::File::create(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        ready.write_all(br#"{"task_id":"ok"}"#).await.unwrap();
        ready.write_all(b"\n").await.unwrap();
        let mut fail = fs::File::create(dir.path().join("failures.ndjson"))
            .await
            .unwrap();
        fail.write_all(br#"{"task_id":"bad"}"#).await.unwrap();
        fail.write_all(b"\n").await.unwrap();
        let ledger = CheckpointLedger::load(dir.path()).await.unwrap();
        let skip = ledger.skip_ids();
        assert!(skip.contains("ok") && skip.contains("bad"));
        let tracker = CheckpointTracker::new(dir.path());
        tracker.seed_from_ledger(&ledger);
        assert_eq!(tracker.snapshot().ok, 1);
        assert_eq!(tracker.snapshot().fail, 1);
    }

    #[tokio::test]
    async fn load_skips_corrupt_lines_and_keeps_valid() {
        let dir = tempfile::tempdir().unwrap();
        let mut ready = fs::File::create(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        ready
            .write_all(
                br#"{"task_id":"t-0","ok":true}
not-json
{"task_id":"t-1","ok":true}
{"no_id":true}
"#,
            )
            .await
            .unwrap();
        let ledger = CheckpointLedger::load(dir.path()).await.unwrap();
        assert!(ledger.ready.contains("t-0"));
        assert!(ledger.ready.contains("t-1"));
        assert_eq!(ledger.ready.len(), 2);
    }
}
