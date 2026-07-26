//! Lightweight pPilot observability: queue / placement / per-task timing.
//!
//! Enabled with `--observe`. Progress lines go to **stderr** (prefix `[obs]`).
//! Machine NDJSON goes to `--observe-file` (and optionally stderr with `--observe-json`).

use crate::scheduler::Scheduler;
use crate::task::unix_now;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPhase {
    Queued,
    Assigned,
    Running,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObsEvent {
    pub kind: &'static str,
    pub ts: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TaskPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loads: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inflight: Option<Vec<InflightView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InflightView {
    pub task_id: String,
    pub worker: usize,
    pub worker_id: String,
    pub phase: TaskPhase,
    pub elapsed_ms: u64,
    pub attempt: u32,
}

struct InflightRec {
    worker: usize,
    worker_id: String,
    phase: TaskPhase,
    since: Instant,
    attempt: u32,
}

/// Shared live view of in-flight tasks + event sink.
pub struct Observer {
    enabled: bool,
    /// Human-readable `[obs] …` lines on stderr.
    human: bool,
    /// NDJSON on stderr (in addition to optional file).
    json_stderr: bool,
    file: AsyncMutex<Option<tokio::fs::File>>,
    inflight: Mutex<HashMap<String, InflightRec>>,
    queued: Mutex<usize>,
}

pub struct ObserverOptions {
    pub human: bool,
    pub json_stderr: bool,
    pub path: Option<std::path::PathBuf>,
}

impl Observer {
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            enabled: false,
            human: false,
            json_stderr: false,
            file: AsyncMutex::new(None),
            inflight: Mutex::new(HashMap::new()),
            queued: Mutex::new(0),
        })
    }

    pub async fn open(opts: ObserverOptions) -> anyhow::Result<Arc<Self>> {
        let file = if let Some(p) = opts.path.as_deref() {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await?;
                }
            }
            Some(OpenOptions::new().create(true).append(true).open(p).await?)
        } else {
            None
        };
        let obs = Arc::new(Self {
            enabled: true,
            human: opts.human,
            json_stderr: opts.json_stderr,
            file: AsyncMutex::new(file),
            inflight: Mutex::new(HashMap::new()),
            queued: Mutex::new(0),
        });
        if obs.human {
            let mut msg = String::from(
                "[obs] enabled — progress on stderr (results default to --results quiet)",
            );
            if let Some(p) = opts.path.as_deref() {
                msg.push_str(&format!("; NDJSON → {}", p.display()));
            }
            if opts.json_stderr {
                msg.push_str("; NDJSON also on stderr (--observe-json)");
            }
            eprintln!("{msg}");
        }
        Ok(obs)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    fn human_line(&self, line: &str) {
        if self.human {
            eprintln!("[obs] {line}");
        }
    }

    async fn emit_json(&self, event: &ObsEvent) {
        if !self.enabled {
            return;
        }
        let mut guard = self.file.lock().await;
        if !self.json_stderr && guard.is_none() {
            return;
        }
        let Ok(line) = serde_json::to_string(event) else {
            return;
        };
        if self.json_stderr {
            eprintln!("{line}");
        }
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes()).await;
            let _ = f.write_all(b"\n").await;
            let _ = f.flush().await;
        }
    }

    async fn emit(&self, event: ObsEvent, human: Option<String>) {
        if !self.enabled {
            return;
        }
        if let Some(h) = human {
            self.human_line(&h);
        }
        self.emit_json(&event).await;
    }

    pub async fn task_queued(&self, task_id: &str) {
        if !self.enabled {
            return;
        }
        let q = {
            let mut g = self.queued.lock().unwrap_or_else(|e| e.into_inner());
            *g += 1;
            *g
        };
        self.emit(
            ObsEvent {
                kind: "task.queued",
                ts: unix_now(),
                task_id: Some(task_id.into()),
                worker: None,
                worker_id: None,
                attempt: None,
                phase: Some(TaskPhase::Queued),
                ok: None,
                duration_ms: None,
                elapsed_ms: None,
                loads: None,
                inflight: None,
                queued: Some(q),
                error: None,
            },
            Some(format!("queued   {task_id}  queue={q}")),
        )
        .await;
    }

    pub async fn task_assigned(
        &self,
        task_id: &str,
        worker: usize,
        worker_id: &str,
        attempt: u32,
        sched: &Scheduler,
    ) {
        if !self.enabled {
            return;
        }
        {
            let mut q = self.queued.lock().unwrap_or_else(|e| e.into_inner());
            *q = q.saturating_sub(1);
        }
        {
            let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            map.insert(
                task_id.to_string(),
                InflightRec {
                    worker,
                    worker_id: worker_id.to_string(),
                    phase: TaskPhase::Assigned,
                    since: Instant::now(),
                    attempt,
                },
            );
        }
        let loads = sched.load_snapshot();
        let q = self.queued_count().unwrap_or(0);
        self.emit(
            ObsEvent {
                kind: "task.assigned",
                ts: unix_now(),
                task_id: Some(task_id.into()),
                worker: Some(worker),
                worker_id: Some(worker_id.into()),
                attempt: Some(attempt),
                phase: Some(TaskPhase::Assigned),
                ok: None,
                duration_ms: None,
                elapsed_ms: None,
                loads: Some(loads.clone()),
                inflight: None,
                queued: Some(q),
                error: None,
            },
            Some(format!(
                "assigned {task_id} → {worker_id}  loads={loads:?} queue={q}"
            )),
        )
        .await;
    }

    pub async fn task_running(&self, task_id: &str) {
        if !self.enabled {
            return;
        }
        let (worker, worker_id, attempt, elapsed_ms) = {
            let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            let Some(rec) = map.get_mut(task_id) else {
                return;
            };
            rec.phase = TaskPhase::Running;
            (
                rec.worker,
                rec.worker_id.clone(),
                rec.attempt,
                rec.since.elapsed().as_millis() as u64,
            )
        };
        self.emit(
            ObsEvent {
                kind: "task.running",
                ts: unix_now(),
                task_id: Some(task_id.into()),
                worker: Some(worker),
                worker_id: Some(worker_id.clone()),
                attempt: Some(attempt),
                phase: Some(TaskPhase::Running),
                ok: None,
                duration_ms: None,
                elapsed_ms: Some(elapsed_ms),
                loads: None,
                inflight: None,
                queued: None,
                error: None,
            },
            Some(format!("running  {task_id} on {worker_id}")),
        )
        .await;
    }

    pub async fn task_finished(
        &self,
        task_id: &str,
        ok: bool,
        cancelled: bool,
        error: Option<String>,
        sched: &Scheduler,
    ) {
        if !self.enabled {
            return;
        }
        let (worker, worker_id, attempt, duration_ms, phase) = {
            let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rec) = map.remove(task_id) {
                let phase = if cancelled {
                    TaskPhase::Cancelled
                } else if ok {
                    TaskPhase::Finished
                } else {
                    TaskPhase::Failed
                };
                (
                    Some(rec.worker),
                    Some(rec.worker_id),
                    Some(rec.attempt),
                    Some(rec.since.elapsed().as_millis() as u64),
                    Some(phase),
                )
            } else {
                (
                    None,
                    None,
                    None,
                    None,
                    Some(if cancelled {
                        TaskPhase::Cancelled
                    } else if ok {
                        TaskPhase::Finished
                    } else {
                        TaskPhase::Failed
                    }),
                )
            }
        };
        let loads = sched.load_snapshot();
        let q = self.queued_count().unwrap_or(0);
        let status = if cancelled {
            "cancelled"
        } else if ok {
            "ok"
        } else {
            "fail"
        };
        let wid = worker_id.as_deref().unwrap_or("?").to_string();
        let dur = duration_ms
            .map(|d| format!("{d}ms"))
            .unwrap_or_else(|| "?".into());
        let err_s = error
            .as_ref()
            .map(|e| format!(" err={e}"))
            .unwrap_or_default();
        let human = format!("finished {task_id} on {wid}  {dur} {status}{err_s}");
        self.emit(
            ObsEvent {
                kind: "task.finished",
                ts: unix_now(),
                task_id: Some(task_id.into()),
                worker,
                worker_id,
                attempt,
                phase,
                ok: Some(ok && !cancelled),
                duration_ms,
                elapsed_ms: duration_ms,
                loads: Some(loads),
                inflight: None,
                queued: Some(q),
                error,
            },
            Some(human),
        )
        .await;
    }

    pub async fn fleet_snapshot(&self, sched: &Scheduler) {
        if !self.enabled {
            return;
        }
        let inflight = {
            let map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            map.iter()
                .map(|(id, rec)| InflightView {
                    task_id: id.clone(),
                    worker: rec.worker,
                    worker_id: rec.worker_id.clone(),
                    phase: rec.phase,
                    elapsed_ms: rec.since.elapsed().as_millis() as u64,
                    attempt: rec.attempt,
                })
                .collect::<Vec<_>>()
        };
        let loads = sched.load_snapshot();
        let q = self.queued_count().unwrap_or(0);
        let n = inflight.len();
        // Keep human fleet lines short: only when something is in flight or queued.
        let human = if n > 0 || q > 0 {
            let sample: Vec<String> = inflight
                .iter()
                .take(4)
                .map(|v| format!("{}@{}:{}ms", v.task_id, v.worker_id, v.elapsed_ms))
                .collect();
            let more = if n > 4 {
                format!(" +{}", n - 4)
            } else {
                String::new()
            };
            Some(format!(
                "fleet    loads={loads:?} queue={q} inflight={n} [{}{more}]",
                sample.join(", ")
            ))
        } else {
            None
        };
        self.emit(
            ObsEvent {
                kind: "fleet.snapshot",
                ts: unix_now(),
                task_id: None,
                worker: None,
                worker_id: None,
                attempt: None,
                phase: None,
                ok: None,
                duration_ms: None,
                elapsed_ms: None,
                loads: Some(loads),
                inflight: Some(inflight),
                queued: Some(q),
                error: None,
            },
            human,
        )
        .await;
    }

    fn queued_count(&self) -> Option<usize> {
        self.queued.lock().ok().map(|g| *g).or(Some(0))
    }
}

/// Background ticker for [`Observer::fleet_snapshot`].
pub fn spawn_snapshot_loop(
    obs: Arc<Observer>,
    sched: Arc<Scheduler>,
    cancel: tokio_util::sync::CancellationToken,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !obs.enabled() {
            return;
        }
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    obs.fleet_snapshot(&sched).await;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::Scheduler;

    #[tokio::test]
    async fn disabled_observer_is_noop() {
        let obs = Observer::disabled();
        assert!(!obs.enabled());
        obs.task_queued("t-0").await;
        let sched = Scheduler::new(1, 1);
        obs.task_assigned("t-0", 0, "w0", 0, &sched).await;
        obs.task_running("t-0").await;
        obs.task_finished("t-0", true, false, None, &sched).await;
        obs.fleet_snapshot(&sched).await;
    }

    #[tokio::test]
    async fn enabled_tracks_queued_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obs.ndjson");
        let obs = Observer::open(ObserverOptions {
            human: false,
            json_stderr: false,
            path: Some(path.clone()),
        })
        .await
        .unwrap();
        obs.task_queued("t-0").await;
        obs.task_queued("t-1").await;
        let body = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(body.contains("task.queued"));
        assert_eq!(body.lines().count(), 2);
    }
}
