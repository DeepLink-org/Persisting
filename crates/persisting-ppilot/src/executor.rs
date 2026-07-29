//! Transitional executor seam — Worker invokes this while pPilot schedules TaskExpr.
//!
//! In the target architecture this implementation becomes a pVisor executor
//! provider; pPilot itself should only submit RunSpec and observe RunFuture.
//!
//! **Primitive:** [`Executor`] trait · [`ExecutorRouter`] (product: `op=execute` only).
//!
//! ```text
//! Driver --ask--> WorkerActor -- op=execute --> plan.py::execute(item)
//! ```

use crate::python_env;
use crate::task::{unix_now, TaskExpr, TaskResult};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Long-lived host: load plan module once, call `execute(item)`.
const PLAN_EXECUTE_HOST: &str = r#"
import importlib.util, json, sys, traceback, types
from pathlib import Path

plan_mods = {}

def install_context(raw):
    # A tiny injected module keeps execute(item) stateless while making
    # placement data available to algorithm code.
    mod = types.ModuleType("persisting_ppilot")
    frozen = json.loads(json.dumps(raw or {}))
    mod.context = lambda: json.loads(json.dumps(frozen))
    sys.modules["persisting_ppilot"] = mod
    # Compatibility for existing plan scripts. New code should import
    # `persisting_ppilot`; this alias can be removed after the migration window.
    sys.modules["persisting_compute"] = mod
    return frozen

def load_plan_module(script, argv=None, context=None):
    script = str(Path(script).resolve())
    if script in plan_mods:
        return plan_mods[script]
    path = Path(script)
    worker_context = install_context(context)
    # Match `python task.py --foo bar` so argparse works at import time.
    sys.argv = [script, *(argv or [])]
    spec = importlib.util.spec_from_file_location("user_plan_exec", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    setup = getattr(mod, "setup_worker", None)
    if setup is not None:
        if not callable(setup):
            raise TypeError("setup_worker must be callable")
        setup(worker_context)
    plan_mods[script] = mod
    return mod

def handle(msg):
    cmd = msg.get("cmd")
    if cmd == "shutdown":
        for mod in plan_mods.values():
            teardown = getattr(mod, "teardown_worker", None)
            if teardown is not None:
                if not callable(teardown):
                    raise TypeError("teardown_worker must be callable")
                teardown()
        return {"ok": True, "value": "bye"}
    if cmd == "run_plan":
        script = msg.get("script")
        if not script:
            raise ValueError("run_plan requires script= path to plan.py")
        mod = load_plan_module(script, msg.get("argv") or [], msg.get("context") or {})
        if not hasattr(mod, "execute"):
            raise AttributeError(
                f"{script} must define execute(item) — same object plan() yields"
            )
        fn = getattr(mod, "execute")
        if not callable(fn):
            raise TypeError("execute must be callable")
        # Pass the same shape plan() yields: {id, ...fields}, not wire TaskExpr.
        task = msg.get("task") or {}
        item = dict(task.get("args") or {})
        if task.get("id") is not None:
            item["id"] = task["id"]
        return {"ok": True, "value": fn(item)}
    raise ValueError(f"unknown cmd: {cmd!r}")

def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req_id = None
        try:
            msg = json.loads(line)
            req_id = msg.get("id")
            out = handle(msg)
            out["id"] = req_id
            print(json.dumps(out, default=str), flush=True)
            if msg.get("cmd") == "shutdown":
                break
        except Exception as e:
            print(json.dumps({
                "id": req_id,
                "ok": False,
                "error": str(e),
                "traceback": traceback.format_exc(),
            }), flush=True)

if __name__ == "__main__":
    main()
"#;

struct PlanHost {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

/// Owns one long-lived `--python` process that caches the loaded plan module.
struct PlanHostExecutor {
    python: PathBuf,
    pythonpath_extra: Vec<PathBuf>,
    worker_context: Value,
    host: Mutex<Option<PlanHost>>,
}

impl PlanHostExecutor {
    fn new(python: PathBuf, pythonpath_extra: Vec<PathBuf>, worker_context: Value) -> Self {
        Self {
            python,
            pythonpath_extra,
            worker_context,
            host: Mutex::new(None),
        }
    }

    async fn ensure_host(
        host: &mut Option<PlanHost>,
        python: &PathBuf,
        pythonpath_extra: &[PathBuf],
    ) -> Result<()> {
        if host.is_some() {
            return Ok(());
        }
        let mut cmd = Command::new(python);
        cmd.arg("-u")
            .arg("-c")
            .arg(PLAN_EXECUTE_HOST)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        python_env::apply_pythonpath(&mut cmd, pythonpath_extra);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn plan execute host: {}", python.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("plan host missing stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("plan host missing stdout"))?;
        *host = Some(PlanHost {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        });
        Ok(())
    }

    async fn request(host: &mut PlanHost, mut msg: Value) -> Result<Value> {
        let id = host.next_id;
        host.next_id += 1;
        msg.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("request must be object"))?
            .insert("id".into(), json!(id));
        let line = serde_json::to_string(&msg)?;
        host.stdin.write_all(line.as_bytes()).await?;
        host.stdin.write_all(b"\n").await?;
        host.stdin.flush().await?;

        let mut reply = String::new();
        let n = host.stdout.read_line(&mut reply).await?;
        if n == 0 {
            bail!("plan execute host closed stdout");
        }
        let parsed: Value = serde_json::from_str(reply.trim())
            .with_context(|| format!("invalid host reply: {}", reply.trim()))?;
        if parsed.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            Ok(parsed.get("value").cloned().unwrap_or(Value::Null))
        } else {
            let err = parsed
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("plan execute failed");
            let tb = parsed
                .get("traceback")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            bail!("{err}\n{tb}")
        }
    }

    async fn shutdown(&self) {
        let mut guard = self.host.lock().await;
        if let Some(mut h) = guard.take() {
            let _ = Self::request(&mut h, json!({"cmd": "shutdown"})).await;
            let _ = h.child.kill().await;
        }
    }

    async fn run_plan_execute(
        &self,
        plan_script: &Path,
        script_args: &[String],
        task: TaskExpr,
        worker_id: &str,
        cancel: CancellationToken,
    ) -> TaskResult {
        let started = unix_now();
        if cancel.is_cancelled() {
            return TaskResult::cancelled(task.id);
        }
        let task_json = match serde_json::to_value(&task) {
            Ok(v) => v,
            Err(e) => {
                return TaskResult::failure(
                    task.id,
                    format!("encode task: {e}"),
                    None,
                    worker_id,
                    started,
                );
            }
        };
        let script = match plan_script.canonicalize() {
            Ok(p) => p,
            Err(_) => plan_script.to_path_buf(),
        };
        let mut guard = self.host.lock().await;
        if let Err(e) = Self::ensure_host(&mut guard, &self.python, &self.pythonpath_extra).await {
            return TaskResult::failure(
                task.id,
                e.to_string(),
                Some(format!("{e:#}")),
                worker_id,
                started,
            );
        }
        let msg = json!({
            "cmd": "run_plan",
            "script": script,
            "argv": script_args,
            "task": task_json,
            "context": self.worker_context,
        });
        // In-flight cancel: kill the Python host so execute does not outlive Ctrl-C.
        let result = {
            let host = guard.as_mut().expect("host just ensured");
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    Err(anyhow::anyhow!("cancelled"))
                }
                r = Self::request(host, msg) => r,
            }
        };
        match result {
            Ok(value) => TaskResult::success(task.id, value, worker_id, started),
            Err(e) if cancel.is_cancelled() || e.to_string().contains("cancelled") => {
                if let Some(mut h) = guard.take() {
                    let _ = h.child.kill().await;
                }
                TaskResult::cancelled(task.id)
            }
            Err(e) => {
                let tb = format!("{e:#}");
                TaskResult::failure(task.id, e.to_string(), Some(tb), worker_id, started)
            }
        }
    }
}

/// Default algo path: plan script's `execute(item)`.
pub struct PlanExecuteExecutor {
    host: Arc<PlanHostExecutor>,
    plan_script: PathBuf,
    script_args: Vec<String>,
}

impl PlanExecuteExecutor {
    async fn run_with_cancel(
        &self,
        task: TaskExpr,
        worker_id: &str,
        cancel: CancellationToken,
    ) -> TaskResult {
        self.host
            .run_plan_execute(
                &self.plan_script,
                &self.script_args,
                task,
                worker_id,
                cancel,
            )
            .await
    }
}

/// Routes `op=execute` to [`PlanExecuteExecutor`]. Unknown ops fail clearly.
pub struct ExecutorRouter {
    execute: Arc<PlanExecuteExecutor>,
    host: Arc<PlanHostExecutor>,
}

impl ExecutorRouter {
    /// Worker stack for the product surface (plan + execute only).
    pub fn local_stack(
        python: PathBuf,
        pythonpath_extra: Vec<PathBuf>,
        plan_script: PathBuf,
        script_args: Vec<String>,
        worker_context: Value,
    ) -> Self {
        let host = Arc::new(PlanHostExecutor::new(
            python,
            pythonpath_extra,
            worker_context,
        ));
        let execute = Arc::new(PlanExecuteExecutor {
            host: Arc::clone(&host),
            plan_script,
            script_args,
        });
        Self { execute, host }
    }

    #[cfg(test)]
    async fn run(&self, task: TaskExpr, worker_id: &str) -> TaskResult {
        self.run_with_cancel(task, worker_id, CancellationToken::new())
            .await
    }

    pub async fn run_with_cancel(
        &self,
        task: TaskExpr,
        worker_id: &str,
        cancel: CancellationToken,
    ) -> TaskResult {
        if task.op != "execute" {
            let started = unix_now();
            return TaskResult::failure(
                task.id,
                format!(
                    "unknown op {:?}: only op=execute (plan.py::execute) is supported",
                    task.op
                ),
                None,
                worker_id,
                started,
            );
        }
        self.execute.run_with_cancel(task, worker_id, cancel).await
    }

    pub async fn shutdown(&self) {
        self.host.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskExpr;
    use serde_json::json;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_with_cancel_kills_slow_execute() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("slow.py");
        std::fs::write(
            &script,
            r#"
import time

def plan():
    yield {"id": "t-0"}

def execute(item):
    time.sleep(5)
    return {"done": True}
"#,
        )
        .unwrap();
        let router = ExecutorRouter::local_stack(
            PathBuf::from("python3"),
            vec![],
            script,
            vec![],
            json!({}),
        );
        let cancel = CancellationToken::new();
        let bg = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            bg.cancel();
        });
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 1})).unwrap();
        let t0 = std::time::Instant::now();
        let r = router.run_with_cancel(task, "w0", cancel).await;
        assert!(r.cancelled, "{r:?}");
        assert!(t0.elapsed().as_secs_f64() < 2.0);
        router.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_execute_returns_value() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ok.py");
        std::fs::write(
            &script,
            r#"
def plan():
    yield {"id": "t-0"}

def execute(item):
    return {"x2": item["x"] * 2}
"#,
        )
        .unwrap();
        let router = ExecutorRouter::local_stack(
            PathBuf::from("python3"),
            vec![],
            script,
            vec![],
            json!({}),
        );
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 3})).unwrap();
        let r = router.run(task, "w0").await;
        assert!(r.ok);
        assert_eq!(r.value, Some(json!({"x2": 6})));
        router.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_context_and_hooks_do_not_change_execute_signature() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("context.py");
        std::fs::write(
            &script,
            r#"
from persisting_ppilot import context
from persisting_compute import context as legacy_context

def setup_worker(ctx):
    assert ctx["worker_id"] == "w-context"
    assert legacy_context()["worker_id"] == "w-context"

def plan():
    yield {"id": "unused"}

def execute(item):
    ctx = context()
    return {"metrics": {"rank": ctx["rank"]}, "artifacts": {"worker": ctx["worker_id"]}}

def teardown_worker():
    pass
"#,
        )
        .unwrap();
        let router = ExecutorRouter::local_stack(
            PathBuf::from("python3"),
            vec![],
            script,
            vec![],
            json!({"worker_id": "w-context", "rank": 3}),
        );
        let task = TaskExpr::from_value(json!({"id": "t-0"})).unwrap();
        let result = router.run(task, "w-context").await;
        assert!(result.ok);
        assert_eq!(result.metrics.get("rank"), Some(&3.0));
        assert_eq!(result.artifacts.get("worker"), Some(&json!("w-context")));
        router.shutdown().await;
    }
}
