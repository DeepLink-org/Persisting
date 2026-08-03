//! Bounded async sink writer — keeps Driver `on_result` off the disk path.
//!
//! Completions are enqueued via async `send` (natural backpressure — no
//! `block_in_place`). A single background task runs [`persist_terminal`] +
//! checkpoint notes. Persist failures are counted and surface from
//! [`SinkWriterHandle::join`]; the task id is unclaimed from [`SkipSet`] so a
//! later `--resume` can rediscover work that never hit durable storage.

use crate::checkpoint::CheckpointTracker;
use crate::coordination::RunCoordinator;
use crate::sink::{persist_terminal, ResultSink};
use crate::skip::SkipSet;
use crate::task::TaskResult;
use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Cloneable enqueue handle for Driver (awaited on the completion path).
#[derive(Clone)]
pub struct SinkSubmitter {
    tx: mpsc::Sender<TaskResult>,
}

impl SinkSubmitter {
    /// Async enqueue — when the bound is full, waits (back-pressures Driver).
    pub async fn submit(&self, result: TaskResult) -> Result<()> {
        self.tx
            .send(result)
            .await
            .map_err(|_| anyhow::anyhow!("sink writer closed; cannot enqueue"))
    }
}

#[derive(Default)]
struct PersistErrors {
    count: AtomicUsize,
    first: Mutex<Option<String>>,
}

impl PersistErrors {
    fn note(&self, task_id: &str, err: impl std::fmt::Display) {
        let prev = self.count.fetch_add(1, Ordering::AcqRel);
        if prev == 0 {
            if let Ok(mut g) = self.first.lock() {
                *g = Some(format!("{task_id}: {err}"));
            }
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    fn first_msg(&self) -> Option<String> {
        self.first.lock().ok().and_then(|g| g.clone())
    }
}

/// Handle returned by [`spawn_sink_writer`]. Await [`SinkWriterHandle::join`] to drain.
pub struct SinkWriterHandle {
    submitter: SinkSubmitter,
    join: JoinHandle<()>,
    errors: Arc<PersistErrors>,
}

impl SinkWriterHandle {
    pub fn submitter(&self) -> SinkSubmitter {
        self.submitter.clone()
    }

    pub async fn submit(&self, result: TaskResult) -> Result<()> {
        self.submitter.submit(result).await
    }

    /// Drop the queue sender, wait for the writer, then fail if any persist errored.
    pub async fn join(self) -> Result<()> {
        drop(self.submitter);
        self.join.await.context("sink writer task join")?;
        let n = self.errors.count();
        if n > 0 {
            let first = self.errors.first_msg().unwrap_or_else(|| "unknown".into());
            bail!("sink persist failed for {n} result(s); first: {first}");
        }
        Ok(())
    }
}

/// Spawn a dedicated persist task. `capacity` bounds queued completions.
pub fn spawn_sink_writer(
    sink: Arc<dyn ResultSink>,
    checkpoint: Option<Arc<CheckpointTracker>>,
    skip: Option<SkipSet>,
    capacity: usize,
) -> SinkWriterHandle {
    spawn_sink_writer_inner(sink, checkpoint, skip, capacity, None)
}

/// Spawn a writer that makes the pChronicle RunCommit authoritative before
/// exposing the result through the user-facing sink.
pub fn spawn_coordinated_sink_writer(
    sink: Arc<dyn ResultSink>,
    checkpoint: Option<Arc<CheckpointTracker>>,
    skip: Option<SkipSet>,
    capacity: usize,
    coordinator: Arc<RunCoordinator>,
) -> SinkWriterHandle {
    spawn_sink_writer_inner(sink, checkpoint, skip, capacity, Some(coordinator))
}

fn spawn_sink_writer_inner(
    sink: Arc<dyn ResultSink>,
    checkpoint: Option<Arc<CheckpointTracker>>,
    skip: Option<SkipSet>,
    capacity: usize,
    coordinator: Option<Arc<RunCoordinator>>,
) -> SinkWriterHandle {
    let capacity = capacity.max(1);
    let (tx, mut rx) = mpsc::channel::<TaskResult>(capacity);
    let errors = Arc::new(PersistErrors::default());
    let errors_bg = Arc::clone(&errors);
    let join = tokio::spawn(async move {
        while let Some(r) = rx.recv().await {
            let persisted = match &coordinator {
                Some(coordinator) => coordinator.finalize_result(sink.as_ref(), &r).await,
                None => persist_terminal(sink.as_ref(), &r).await,
            };
            if let Err(e) = persisted {
                tracing::error!(task_id = %r.task_id, error = %e, "sink persist failed");
                errors_bg.note(&r.task_id, &e);
                // Not durable — allow a future resume / duplicate plan yield to reclaim.
                if let Some(skip) = &skip {
                    skip.remove(&r.task_id);
                }
                continue;
            }
            if let Some(ckpt) = &checkpoint {
                ckpt.note_terminal(r.ok, r.cancelled);
                let _ = ckpt.maybe_flush().await;
            }
        }
    });
    SinkWriterHandle {
        submitter: SinkSubmitter { tx },
        join,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::JsonlFileSink;
    use async_trait::async_trait;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_persists_and_drains() {
        let dir = tempfile::tempdir().unwrap();
        let sink: Arc<dyn ResultSink> = Arc::new(JsonlFileSink::open(dir.path()).await.unwrap());
        let w = spawn_sink_writer(Arc::clone(&sink), None, None, 8);
        w.submit(TaskResult::success("t-0", json!(1), "w0", 0.0))
            .await
            .unwrap();
        w.submit(TaskResult::failure("t-1", "e", None, "w0", 0.0))
            .await
            .unwrap();
        w.join().await.unwrap();
        let ready = tokio::fs::read_to_string(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        let fail = tokio::fs::read_to_string(dir.path().join("failures.ndjson"))
            .await
            .unwrap();
        assert!(ready.contains("t-0"));
        assert!(fail.contains("t-1"));
    }

    struct AlwaysFailSink;

    #[async_trait]
    impl ResultSink for AlwaysFailSink {
        async fn append_ready(&self, _: &TaskResult) -> Result<()> {
            bail!("disk full")
        }
        async fn append_failure(&self, _: &TaskResult) -> Result<()> {
            bail!("disk full")
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persist_failure_surfaces_on_join_and_unclaims_skip() {
        let skip: SkipSet = ["t-0".into()].into_iter().collect();
        assert!(skip.contains("t-0"));
        let w = spawn_sink_writer(Arc::new(AlwaysFailSink), None, Some(skip.clone()), 4);
        w.submit(TaskResult::success("t-0", json!(1), "w0", 0.0))
            .await
            .unwrap();
        let err = w.join().await.unwrap_err();
        assert!(err.to_string().contains("sink persist failed"));
        assert!(
            !skip.contains("t-0"),
            "failed persist must unclaim so resume can rediscover"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submit_is_async_backpressure_not_block_in_place() {
        // Smoke: async send path works under modest load (no block_in_place).
        let dir = tempfile::tempdir().unwrap();
        let sink: Arc<dyn ResultSink> = Arc::new(JsonlFileSink::open(dir.path()).await.unwrap());
        let w = spawn_sink_writer(Arc::clone(&sink), None, None, 2);
        for i in 0..4 {
            w.submit(TaskResult::success(format!("t-{i}"), json!(i), "w0", 0.0))
                .await
                .unwrap();
        }
        // Must not retain submitter clones across join (keeps writer channel open).
        w.join().await.unwrap();
        let ready = tokio::fs::read_to_string(dir.path().join("ready.ndjson"))
            .await
            .unwrap();
        assert_eq!(ready.lines().count(), 4);
    }
}
