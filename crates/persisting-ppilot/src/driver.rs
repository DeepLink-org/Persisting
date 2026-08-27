//! Driver — control plane that drives the worker fleet.
//!
//! Conceptual ownership (one Driver per job on rank0 / local control process):
//! 1. **Plan** — stream tasks from the user `plan()` script
//! 2. **Dispatch** — least-loaded placement; `ask` workers **directly** (parallel)
//! 3. **Drain** — complete → `on_result` immediately; at most `max_inflight` JoinHandles
//!
//! This is a Rust control object, not a Pulsing Actor that `await`s Execute in
//! `receive` (that would serialize the fleet). Workers remain Pulsing actors.

use crate::checkpoint::CheckpointTracker;
use crate::coordination::RunCoordinator;
use crate::executor::task_run_spec;
use crate::future::RunFuture;
use crate::observe::Observer;
use crate::plan::stream_plan_tasks;
use crate::pulsing_ext::{ASK_TIMEOUT, ask_timeout};
use crate::scheduler::{AcquireError, Scheduler, StickyLost, WorkerPool};
use crate::sink_writer::SinkSubmitter;
use crate::skip::SkipSet;
use crate::task::{ErrorKind, TaskExpr, TaskResult, unix_now};
use crate::worker::{WorkerCommand, WorkerReply};
use anyhow::Result;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Job knobs owned/consumed by the Driver when running a plan.
#[derive(Clone)]
pub struct RunOptions {
    pub script: PathBuf,
    pub python: PathBuf,
    /// Standalone pVisor executable used for every Run.
    pub pvisor_binary: PathBuf,
    /// Local-only worker count when not under torchrun.
    pub workers: usize,
    /// Global cap on concurrent tasks (defaults to `workers * per_worker_inflight`).
    /// Also bounds outstanding JoinHandles (complete → drop).
    pub max_inflight: usize,
    /// Max concurrent Executes per worker (least-loaded scheduling). Default 1.
    pub per_worker_inflight: usize,
    /// Extra entries prepended onto PYTHONPATH for plan / execute host.
    pub pythonpath_extra: Vec<PathBuf>,
    /// Forwarded to task.py as ``sys.argv[1:]`` (argparse-friendly).
    pub script_args: Vec<String>,
    /// pPilot infrastructure retries on worker ask failure (default 2).
    pub infra_retries: u32,
    /// Job-level cancel (Ctrl-C / external). Child tokens per in-flight task.
    pub job_cancel: CancellationToken,
    /// Optional observability sink (`--observe`).
    pub observer: Arc<Observer>,
    /// Task ids to skip / already claimed (`--resume` seed + live completions).
    pub skip_task_ids: SkipSet,
    /// Optional checkpoint progress (`checkpoint.json` under `--sink`).
    pub checkpoint: Option<Arc<CheckpointTracker>>,
    /// Optional async sink enqueue (awaited on completion — back-pressures drain).
    pub sink_submitter: Option<SinkSubmitter>,
    /// Durable lease/commit control. When present every accepted task receives
    /// one fencing epoch before it can contact a worker.
    pub coordinator: Option<Arc<RunCoordinator>>,
}

/// Drives one pPilot job: emit plan tasks and dispatch them onto the fleet.
pub struct Driver {
    pool: WorkerPool,
    sched: Arc<Scheduler>,
}

impl Driver {
    pub fn new(pool: WorkerPool, sched: Arc<Scheduler>) -> Self {
        Self { pool, sched }
    }

    pub fn scheduler(&self) -> &Arc<Scheduler> {
        &self.sched
    }

    pub fn pool(&self) -> &WorkerPool {
        &self.pool
    }

    /// Run user `plan()` → place each task on a worker → collect results.
    ///
    /// Completions call `on_result` in **finish order** (not plan emit order).
    /// At most `max_inflight` [`RunFuture`]s are outstanding at once.
    pub async fn run_plan(
        &self,
        opts: &RunOptions,
        mut on_result: impl FnMut(TaskResult) + Send,
    ) -> Result<Vec<TaskResult>> {
        if opts.coordinator.is_some() && opts.sink_submitter.is_none() {
            anyhow::bail!("RunCoordinator requires a coordinated sink writer");
        }
        let global_cap = opts.max_inflight.max(1).min(self.sched.capacity().max(1));
        let mut stream = stream_plan_tasks(
            opts.script.clone(),
            opts.python.clone(),
            opts.script_args.clone(),
        );
        // Each element is a RunFuture::wait(); len() bounds outstanding JoinHandles.
        let mut inflight: FuturesUnordered<
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<TaskResult>> + Send>>,
        > = FuturesUnordered::new();
        let mut out = Vec::new();
        let mut plan_done = false;
        let retries = opts.infra_retries;
        let observer = Arc::clone(&opts.observer);
        let coordinator = opts.coordinator.clone();

        loop {
            // Guard before select: empty set + plan_done would disable every arm.
            if plan_done && inflight.is_empty() {
                break;
            }

            tokio::select! {
                biased;

                // Prefer draining completions so sink / resume stay current.
                Some(joined) = inflight.next(), if !inflight.is_empty() => {
                    let r: TaskResult = joined?;
                    on_result(r.clone());
                    if let Some(sink) = &opts.sink_submitter {
                        sink.submit(r.clone())
                            .await
                            .map_err(|e| anyhow::anyhow!("sink enqueue: {e}"))?;
                    }
                    out.push(r);
                }

                item = stream.next(), if !plan_done
                    && inflight.len() < global_cap
                    && !opts.job_cancel.is_cancelled() =>
                {
                    match item {
                        None => plan_done = true,
                        Some(Err(e)) => return Err(e),
                        Some(Ok(task)) => {
                            let task_id = task.id.clone();
                            // Claim before dispatch: resume seed, live terminals, and
                            // duplicate plan() yields share one skip set (cross-worker
                            // re-dispatch of a finished id is skipped).
                            if !opts.skip_task_ids.insert(task_id.clone()) {
                                if let Some(ckpt) = &opts.checkpoint {
                                    ckpt.note_skipped(1);
                                    let _ = ckpt.maybe_flush().await;
                                }
                                continue;
                            }
                            observer.task_queued(&task_id).await;
                            if let Some(ckpt) = &opts.checkpoint {
                                ckpt.note_dispatched();
                            }
                            let pool = Arc::clone(&self.pool);
                            let sched = Arc::clone(&self.sched);
                            let observer = Arc::clone(&observer);
                            let coordinator = coordinator.clone();
                            let cancel = opts.job_cancel.child_token();
                            let cancel_watch = cancel.clone();
                            let join = tokio::spawn(async move {
                                execute_with_placement(
                                    pool,
                                    sched,
                                    observer,
                                    task,
                                    retries,
                                    cancel_watch,
                                    coordinator,
                                )
                                .await
                            });
                            inflight.push(Box::pin(RunFuture::new(task_id, join, cancel).wait()));
                        }
                    }
                }

                _ = opts.job_cancel.cancelled(), if !plan_done => {
                    tracing::debug!("job cancel: stop accepting new plan tasks");
                    plan_done = true;
                }
            }
        }

        Ok(out)
    }
}

async fn execute_with_placement(
    pool: WorkerPool,
    sched: Arc<Scheduler>,
    observer: Arc<Observer>,
    task: TaskExpr,
    infra_retries: u32,
    cancel: CancellationToken,
    coordinator: Option<Arc<RunCoordinator>>,
) -> Result<TaskResult> {
    let task_id = task.id.clone();
    let started = unix_now();
    let run_id = task_run_spec(&task, "unassigned", 0).run_id;
    let lease_epoch = match &coordinator {
        Some(coordinator) => {
            coordinator
                .acquire_lease(&run_id, &task_id, coordinator.owner_id())
                .await?
        }
        None => 1,
    };
    let heartbeat = coordinator
        .as_ref()
        .map(|coordinator| {
            coordinator.start_lease_heartbeat(run_id.clone(), lease_epoch, cancel.clone())
        })
        .transpose()?;
    let outcome = async {
    let task_json = serde_json::to_vec(&task).map_err(|e| anyhow::anyhow!("encode task: {e}"))?;
    let mut last_err = None;
    // After first Execute contact: stick forever (result_cache is per-slot).
    // Never fall through to another slot — that would be at-least-once re-execute.
    let mut sticky: Option<usize> = None;

    for attempt in 0..=infra_retries {
        if cancel.is_cancelled() {
            observer
                .task_finished(&task_id, false, true, None, &sched)
                .await;
            return Ok(stamp_control(
                TaskResult::cancelled(task_id),
                &run_id,
                lease_epoch,
            ));
        }
        let guard = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                observer
                    .task_finished(&task_id, false, true, None, &sched)
                    .await;
                return Ok(stamp_control(
                    TaskResult::cancelled(task_id),
                    &run_id,
                    lease_epoch,
                ));
            }
            g = async {
                match sticky {
                    Some(slot) => sched
                        .acquire_guard_sticky(slot)
                        .await
                        .map_err(PlacementErr::Sticky),
                    None => sched
                        .acquire_guard_prefer(None)
                        .await
                        .map_err(PlacementErr::AllGone),
                }
            } => g,
        };
        let guard = match guard {
            Ok(g) => g,
            Err(PlacementErr::Sticky(StickyLost::Quarantined(slot))) => {
                let err = format!(
                    "sticky slot {slot} quarantined after contact (refuse cross-slot re-execute)"
                );
                tracing::warn!(%task_id, %err);
                observer
                    .task_finished(&task_id, false, false, Some(err.clone()), &sched)
                    .await;
                let mut r = TaskResult::failure_with_kind(
                    task_id,
                    format!("infra: {err}"),
                    None,
                    "infra",
                    started,
                    ErrorKind::Infra,
                    true,
                );
                r.infra_retries = attempt;
                return Ok(stamp_control(r, &run_id, lease_epoch));
            }
            Err(PlacementErr::AllGone(AcquireError::AllQuarantined)) => {
                let err = "all worker slots quarantined".to_string();
                tracing::error!(%task_id, %err);
                observer
                    .task_finished(&task_id, false, false, Some(err.clone()), &sched)
                    .await;
                let mut r = TaskResult::failure_with_kind(
                    task_id,
                    format!("infra: {err}"),
                    None,
                    "infra",
                    started,
                    ErrorKind::Infra,
                    true,
                );
                r.infra_retries = attempt;
                return Ok(stamp_control(r, &run_id, lease_epoch));
            }
        };
        let idx = guard.index();
        let worker_id = format!("w{idx}");
        observer
            .task_assigned(&task_id, idx, &worker_id, attempt, &sched)
            .await;
        let worker = {
            let g = pool
                .read()
                .map_err(|_| anyhow::anyhow!("worker pool lock"))?;
            g.get(idx)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("worker index {idx} out of range"))?
        };

        observer.task_running(&task_id).await;
        // Contacted this slot: subsequent infra retries must stay here.
        sticky = Some(idx);
        let ask = ask_timeout::<_, WorkerReply>(
            &worker,
            WorkerCommand::Execute {
                task_json: task_json.clone(),
                lease_epoch,
            },
            ASK_TIMEOUT,
        );
        tokio::pin!(ask);
        let reply = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Local workers share job_cancel; remote ranks get Cancel via JobControlActor.
                drop(guard);
                observer
                    .task_finished(&task_id, false, true, None, &sched)
                    .await;
                return Ok(stamp_control(
                    TaskResult::cancelled(task_id),
                    &run_id,
                    lease_epoch,
                ));
            }
            r = &mut ask => r,
        };
        match reply {
            Ok(WorkerReply::Result { result_json }) => {
                let mut r: TaskResult = serde_json::from_slice(&result_json)
                    .map_err(|e| anyhow::anyhow!("decode result: {e}"))?;
                if attempt > 0 {
                    r.infra_retries = attempt;
                    tracing::debug!(%task_id, attempt, worker = idx, "infra retry succeeded");
                }
                if r.worker.is_none() {
                    r.worker = Some(format!("w{idx}"));
                }
                if r.run_id.as_deref() != Some(run_id.as_str())
                    || r.lease_epoch != lease_epoch
                    || r.attempt_id.is_none()
                {
                    let detail = format!(
                        "worker returned invalid Run identity: run={:?}, attempt={:?}, epoch={} (expected run={}, epoch={}); worker error={:?}; traceback={:?}",
                        r.run_id,
                        r.attempt_id,
                        r.lease_epoch,
                        run_id,
                        lease_epoch,
                        r.error,
                        r.traceback,
                    );
                    r = TaskResult::failure_with_kind(
                        task_id.clone(),
                        detail,
                        None,
                        &worker_id,
                        started,
                        ErrorKind::Infra,
                        true,
                    );
                    r = stamp_control(r, &run_id, lease_epoch);
                }
                sched.note_success(idx);
                observer
                    .task_finished(&task_id, r.ok, r.cancelled, r.error.clone(), &sched)
                    .await;
                drop(guard);
                return Ok(r);
            }
            Ok(WorkerReply::Bye) => {
                last_err = Some("unexpected Bye on Execute".into());
                sched.note_failure(idx);
                drop(guard);
            }
            Err(e) => {
                tracing::warn!(
                    %task_id,
                    attempt,
                    worker = idx,
                    error = %e,
                    "infra retry: worker ask failed (sticky-only re-ask)"
                );
                last_err = Some(e.to_string());
                sched.note_failure(idx);
                drop(guard);
            }
        }
    }

    let err = last_err.unwrap_or_else(|| "unknown".into());
    observer
        .task_finished(&task_id, false, false, Some(err.clone()), &sched)
        .await;
    let mut r = TaskResult::failure_with_kind(
        task_id,
        format!("infra retries exhausted: {err}"),
        None,
        "infra",
        started,
        ErrorKind::Infra,
        true,
    );
    r.infra_retries = infra_retries;
        Ok(stamp_control(r, &run_id, lease_epoch))
    }
    .await;
    if outcome.is_ok()
        && let Some(heartbeat) = heartbeat
    {
        heartbeat.detach();
    }
    outcome
}

fn stamp_control(
    mut result: TaskResult,
    run_id: &persisting_agentctl::RunId,
    epoch: u64,
) -> TaskResult {
    result.run_id = Some(run_id.as_str().to_string());
    result.lease_epoch = epoch;
    if result.attempt_id.is_none() {
        result.attempt_id = Some(format!("ppilot-control-{}-{epoch}", result.task_id));
    }
    result
}

enum PlacementErr {
    Sticky(StickyLost),
    AllGone(AcquireError),
}
