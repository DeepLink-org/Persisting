//! RunFuture — L2 scheduling atom (TaskSpec → wait / cancel).
//!
//! Cancel is cooperative at dispatch / acquire, and in-flight Python is killed
//! via the shared job [`CancellationToken`] watched by the plan execute host.
//! `wait` always joins and **never rewrites a successful result** as cancelled.

use crate::task::TaskResult;
use anyhow::{Context, Result};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// One submitted task under L2 control.
pub struct RunFuture {
    task_id: String,
    join: JoinHandle<Result<TaskResult>>,
    cancel: CancellationToken,
}

impl RunFuture {
    pub(crate) fn new(
        task_id: String,
        join: JoinHandle<Result<TaskResult>>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            task_id,
            join,
            cancel,
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Best-effort cancel: signals placement + shared job token (host kill).
    /// Under torchrun, Driver also broadcasts to each rank's job-control actor.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Wait until the task finishes. Successful results are kept even if cancel raced.
    pub async fn wait(self) -> Result<TaskResult> {
        let task_id = self.task_id.clone();
        let joined = self.join.await.context("run future join")?;
        match joined {
            Ok(r) => Ok(r),
            Err(e) => Err(e).with_context(|| format!("task {task_id} failed")),
        }
    }
}

/// Wait for many futures in **submission** order.
///
/// Prefer [`crate::driver::Driver::run_plan`] for jobs: it drains in completion
/// order and bounds outstanding work. This helper is for callers that need a
/// stable result order matching the input vec.
pub async fn wait_all(futures: Vec<RunFuture>) -> Result<Vec<TaskResult>> {
    let mut out = Vec::with_capacity(futures.len());
    for f in futures {
        out.push(f.wait().await?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn wait_keeps_success_even_if_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        let join =
            tokio::spawn(async { Ok(TaskResult::success("t-1", json!({"x": 1}), "w0", 0.0)) });
        let fut = RunFuture::new("t-1".into(), join, token);
        assert!(fut.is_cancelled());
        let r = fut.wait().await.unwrap();
        assert!(r.ok);
        assert!(!r.cancelled);
        assert_eq!(r.task_id, "t-1");
    }

    #[tokio::test]
    async fn wait_all_preserves_submission_order() {
        let mk = |id: &str, delay_ms: u64| {
            let id = id.to_string();
            let token = CancellationToken::new();
            let id_for_join = id.clone();
            let join = tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                Ok(TaskResult::success(id_for_join, json!(1), "w0", 0.0))
            });
            RunFuture::new(id, join, token)
        };
        // Slow first, fast second — wait_all still returns submission order.
        let out = wait_all(vec![mk("a", 40), mk("b", 1)]).await.unwrap();
        assert_eq!(out[0].task_id, "a");
        assert_eq!(out[1].task_id, "b");
    }
}
