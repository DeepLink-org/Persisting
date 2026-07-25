//! Pulsing WorkerActor — dispatches TaskExpr to plan.py `execute(item)`.
//!
//! Semantic block: **fleet worker seam** (see [`crate::blocks`]).
//! - Shares a job [`CancellationToken`]: in-flight execute kills the Python host.
//! - Uses [`crate::result_cache::ResultCache`] for same-worker infra-retry idempotency.
//! - Spawned via factory + [`SupervisionSpec`] so Pulsing can restart a failed slot.

use crate::executor::ExecutorRouter;
use crate::result_cache::{ResultCache, DEFAULT_RESULT_CACHE_CAP};
use crate::task::{TaskExpr, TaskResult};
use pulsing_actor::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkerCommand {
    Execute { task_json: Vec<u8> },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkerReply {
    Result { result_json: Vec<u8> },
    Bye,
}

/// Shared gate so supervised restarts still signal rank shutdown.
#[derive(Debug)]
pub struct ShutdownGate {
    remaining: AtomicUsize,
    done: Notify,
}

impl ShutdownGate {
    pub fn new(slots: usize) -> Arc<Self> {
        Arc::new(Self {
            remaining: AtomicUsize::new(slots.max(1)),
            done: Notify::new(),
        })
    }

    pub fn note_shutdown(&self) {
        let prev = self.remaining.fetch_sub(1, Ordering::AcqRel);
        if prev <= 1 {
            self.remaining.store(0, Ordering::Release);
            self.done.notify_waiters();
        }
    }

    pub async fn wait(&self) {
        loop {
            if self.remaining.load(Ordering::Acquire) == 0 {
                return;
            }
            self.done.notified().await;
        }
    }
}

/// Cloneable config for supervised `spawn_factory`.
///
/// [`result_cache`] is shared across supervised restarts so sticky infra
/// re-ask still hits cached TaskResults after the actor is rebuilt.
#[derive(Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub python: PathBuf,
    pub pythonpath_extra: Vec<PathBuf>,
    pub plan_script: PathBuf,
    pub script_args: Vec<String>,
    pub job_cancel: CancellationToken,
    pub shutdown_gate: Option<Arc<ShutdownGate>>,
    /// Slot-scoped cache; one Arc per logical slot, shared by factory rebuilds.
    pub result_cache: Arc<Mutex<ResultCache>>,
}

impl WorkerConfig {
    pub fn with_fresh_cache(
        worker_id: impl Into<String>,
        python: PathBuf,
        pythonpath_extra: Vec<PathBuf>,
        plan_script: PathBuf,
        script_args: Vec<String>,
        job_cancel: CancellationToken,
        shutdown_gate: Option<Arc<ShutdownGate>>,
    ) -> Self {
        Self {
            worker_id: worker_id.into(),
            python,
            pythonpath_extra,
            plan_script,
            script_args,
            job_cancel,
            shutdown_gate,
            result_cache: Arc::new(Mutex::new(ResultCache::new(DEFAULT_RESULT_CACHE_CAP))),
        }
    }

    pub fn build(&self) -> WorkerActor {
        WorkerActor {
            worker_id: self.worker_id.clone(),
            executors: Arc::new(ExecutorRouter::local_stack(
                self.python.clone(),
                self.pythonpath_extra.clone(),
                self.plan_script.clone(),
                self.script_args.clone(),
                worker_context(&self.worker_id),
            )),
            done: 0,
            shutdown_gate: self.shutdown_gate.clone(),
            job_cancel: self.job_cancel.clone(),
            result_cache: Arc::clone(&self.result_cache),
        }
    }
}

/// Process-local placement information exposed to Python as
/// `persisting_compute.context()`. `execute(item)` remains stateless.
fn worker_context(worker_id: &str) -> serde_json::Value {
    let rank = std::env::var("RANK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let local_rank = std::env::var("LOCAL_RANK")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(rank);
    let device = if std::env::var_os("LOCAL_RANK").is_some() {
        format!("cuda:{local_rank}")
    } else {
        "cpu".to_string()
    };
    let labels: Vec<_> = std::env::var("PERSISTING_COMPUTE_WORKER_LABELS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .collect();
    json!({
        "worker_id": worker_id,
        "rank": rank,
        "local_rank": local_rank,
        "device": device,
        "job_id": std::env::var("PERSISTING_COMPUTE_JOB_ID").unwrap_or_else(|_| "local".into()),
        "output_dir": std::env::var("PERSISTING_COMPUTE_OUTPUT_DIR").ok(),
        "labels": labels,
    })
}

pub struct WorkerActor {
    pub worker_id: String,
    executors: Arc<ExecutorRouter>,
    done: u64,
    shutdown_gate: Option<Arc<ShutdownGate>>,
    job_cancel: CancellationToken,
    result_cache: Arc<Mutex<ResultCache>>,
}

impl WorkerActor {
    pub fn with_plan(
        worker_id: impl Into<String>,
        python: PathBuf,
        pythonpath_extra: Vec<PathBuf>,
        plan_script: PathBuf,
        script_args: Vec<String>,
        job_cancel: CancellationToken,
    ) -> Self {
        WorkerConfig::with_fresh_cache(
            worker_id,
            python,
            pythonpath_extra,
            plan_script,
            script_args,
            job_cancel,
            None,
        )
        .build()
    }

    pub fn from_config(cfg: &WorkerConfig) -> Self {
        cfg.build()
    }

    async fn execute(&mut self, task: TaskExpr) -> TaskResult {
        if let Ok(g) = self.result_cache.lock() {
            if let Some(cached) = g.get(&task.id) {
                tracing::debug!(
                    task_id = %task.id,
                    worker = %self.worker_id,
                    "infra idempotency: returning cached TaskResult"
                );
                return cached.clone();
            }
        }
        let task_id = task.id.clone();
        let r = self
            .executors
            .run_with_cancel(task, &self.worker_id, self.job_cancel.clone())
            .await;
        if r.ok {
            self.done += 1;
        }
        if let Ok(mut g) = self.result_cache.lock() {
            g.put(task_id, r.clone());
        }
        r
    }
}

#[async_trait]
impl Actor for WorkerActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("role".into(), "compute-worker".into()),
            ("worker_id".into(), self.worker_id.clone()),
            ("done".into(), self.done.to_string()),
        ])
    }

    async fn receive(
        &mut self,
        msg: Message,
        _ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        let cmd: WorkerCommand = msg.unpack()?;
        let reply = match cmd {
            WorkerCommand::Shutdown => {
                self.executors.shutdown().await;
                if let Some(gate) = &self.shutdown_gate {
                    gate.note_shutdown();
                }
                WorkerReply::Bye
            }
            WorkerCommand::Execute { task_json } => {
                let task: TaskExpr = serde_json::from_slice(&task_json).map_err(|e| {
                    pulsing_actor::error::PulsingError::from(
                        pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                    )
                })?;
                let result = self.execute(task).await;
                let result_json = serde_json::to_vec(&result).map_err(|e| {
                    pulsing_actor::error::PulsingError::from(
                        pulsing_actor::error::RuntimeError::Serialization(e.to_string()),
                    )
                })?;
                WorkerReply::Result { result_json }
            }
        };
        Message::pack(&reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn result_cache_avoids_second_execute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("count.py");
        let counter = dir.path().join("counter.txt");
        let counter_lit = counter.display().to_string();
        std::fs::write(
            &path,
            format!(
                r#"
COUNTER = {counter_lit:?}

def plan():
    yield {{"id": "unused"}}

def execute(item):
    with open(COUNTER, "a") as f:
        f.write("run\n")
    return {{"ok": True}}
"#
            ),
        )
        .unwrap();
        let token = CancellationToken::new();
        let system = Arc::new(
            ActorSystem::builder()
                .mailbox_capacity(16)
                .build()
                .await
                .unwrap(),
        );
        let cfg = WorkerConfig {
            worker_id: "w0".into(),
            python: PathBuf::from("python3"),
            pythonpath_extra: vec![],
            plan_script: path,
            script_args: vec![],
            job_cancel: token,
            shutdown_gate: None,
            result_cache: Arc::new(Mutex::new(ResultCache::new(DEFAULT_RESULT_CACHE_CAP))),
        };
        let w = crate::pulsing_ext::spawn_supervised(&system, "compute/worker/0", move || {
            Ok(cfg.build())
        })
        .await
        .unwrap();
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 1})).unwrap();
        let task_json = serde_json::to_vec(&task).unwrap();
        for _ in 0..2 {
            let reply = w
                .ask::<_, WorkerReply>(WorkerCommand::Execute {
                    task_json: task_json.clone(),
                })
                .await
                .unwrap();
            assert!(matches!(reply, WorkerReply::Result { .. }));
        }
        let runs = std::fs::read_to_string(&counter).unwrap();
        assert_eq!(runs.lines().count(), 1, "second ask must hit result cache");
        system.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_gate_fires_when_all_slots_done() {
        let gate = ShutdownGate::new(2);
        let g = Arc::clone(&gate);
        let h = tokio::spawn(async move {
            g.wait().await;
        });
        gate.note_shutdown();
        assert!(!h.is_finished());
        gate.note_shutdown();
        h.await.unwrap();
    }

    #[test]
    fn shared_result_cache_survives_rebuild() {
        let cache = Arc::new(Mutex::new(ResultCache::new(8)));
        let cfg = WorkerConfig {
            worker_id: "w0".into(),
            python: PathBuf::from("python3"),
            pythonpath_extra: vec![],
            plan_script: PathBuf::from("/dev/null"),
            script_args: vec![],
            job_cancel: CancellationToken::new(),
            shutdown_gate: None,
            result_cache: Arc::clone(&cache),
        };
        let _a = cfg.build();
        cache
            .lock()
            .unwrap()
            .put("t-0", TaskResult::success("t-0", json!(1), "w0", 0.0));
        // Supervised restart = factory rebuild; same Arc must still hold entries.
        let _b = cfg.build();
        assert!(cache.lock().unwrap().get("t-0").is_some());
        assert!(Arc::ptr_eq(&cfg.result_cache, &cache));
    }
}
