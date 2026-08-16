//! External pVisor process provider used by pPilot workers while the Driver schedules TaskExpr.
//!
//! Every TaskExpr is adapted to one stable RunSpec. The long-lived Python host
//! is reached through a small loopback client process, so execution,
//! cancellation and terminal state pass through the standalone pVisor binary
//! without paying one Python module import per task.
//!
//! **Primitive:** [`Executor`] trait · [`ExecutorRouter`] (product: `op=execute` only).
//!
//! ```text
//! Driver --ask--> WorkerActor -- RunSpec --> pVisor --> plan.py::execute(item)
//! ```

use crate::python_env;
use crate::task::{unix_now, ErrorKind, TaskExpr, TaskResult};
use anyhow::{bail, Context, Result};
#[cfg(test)]
use persisting_agentctl::{ArtifactRef, ProcessOutput};
use persisting_agentctl::{
    PVisorProcessClient, PVisorProcessOptions, RunFailure, RunFailureKind, RunInvocation,
    RunResult, RunSpec, RunState, StdioMode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(test)]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
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

/// Short-lived workload process launched by the standalone pVisor. It only
/// relays one task to the worker-local Python host and emits one TaskResult.
const PLAN_HOST_CLIENT: &str = r#"
import json, socket, sys

endpoint, token, task_file, worker_id = sys.argv[1:5]
host, port = endpoint.rsplit(":", 1)
with open(task_file, "r", encoding="utf-8") as f:
    task = json.load(f)
with socket.create_connection((host, int(port)), timeout=5) as sock:
    stream = sock.makefile("rwb", buffering=0)
    request = json.dumps({"token": token, "task": task, "worker_id": worker_id})
    stream.write(request.encode("utf-8") + b"\n")
    reply = stream.readline()
    if not reply:
        raise RuntimeError("pPilot plan host closed without a result")
result = json.loads(reply)["result"]
print(json.dumps(result, separators=(",", ":")), flush=True)
if result.get("cancelled"):
    raise SystemExit(130)
if not result.get("ok"):
    raise SystemExit(1)
"#;

#[derive(Debug, Deserialize)]
struct PlanHostRequest {
    token: String,
    task: TaskExpr,
    worker_id: String,
}

#[derive(Debug, Serialize)]
struct PlanHostResponse {
    result: TaskResult,
}

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

struct PlanHostService {
    endpoint: String,
    token: String,
    task_dir: tempfile::TempDir,
    stop: CancellationToken,
    join: JoinHandle<()>,
}

impl PlanHostService {
    fn start(
        host: Arc<PlanHostExecutor>,
        plan_script: PathBuf,
        script_args: Vec<String>,
    ) -> Result<Self> {
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").context("bind pPilot plan-host relay")?;
        listener.set_nonblocking(true)?;
        let endpoint = listener.local_addr()?.to_string();
        let listener = TcpListener::from_std(listener)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let task_dir = tempfile::Builder::new()
            .prefix("persisting-ppilot-worker-")
            .tempdir()?;
        let stop = CancellationToken::new();
        let task_stop = stop.clone();
        let expected_token = token.clone();
        let join = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    _ = task_stop.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let Ok((stream, _)) = accepted else { continue };
                let connection_host = Arc::clone(&host);
                let connection_script = plan_script.clone();
                let connection_args = script_args.clone();
                let connection_token = expected_token.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_plan_host_connection(
                        stream,
                        connection_host,
                        connection_script,
                        connection_args,
                        connection_token,
                    )
                    .await
                    {
                        tracing::debug!(%error, "pPilot plan-host relay ended");
                    }
                });
            }
        });
        Ok(Self {
            endpoint,
            token,
            task_dir,
            stop,
            join,
        })
    }

    async fn write_task(&self, task: &TaskExpr) -> Result<PathBuf> {
        let path = self
            .task_dir
            .path()
            .join(format!("task-{}.json", uuid::Uuid::new_v4().simple()));
        tokio::fs::write(&path, serde_json::to_vec(task)?)
            .await
            .with_context(|| format!("write pPilot task relay file {}", path.display()))?;
        Ok(path)
    }
}

async fn handle_plan_host_connection(
    stream: TcpStream,
    host: Arc<PlanHostExecutor>,
    plan_script: PathBuf,
    script_args: Vec<String>,
    expected_token: String,
) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let line = lines
        .next_line()
        .await?
        .context("plan-host client closed before request")?;
    let request: PlanHostRequest = serde_json::from_str(&line)?;
    if request.token != expected_token {
        bail!("plan-host authentication failed");
    }
    let cancellation = CancellationToken::new();
    let execution = host.run_plan_execute(
        &plan_script,
        &script_args,
        request.task,
        &request.worker_id,
        cancellation.clone(),
    );
    tokio::pin!(execution);
    let result = tokio::select! {
        result = &mut execution => result,
        disconnected = lines.next_line() => {
            cancellation.cancel();
            let _ = disconnected;
            execution.await
        }
    };
    write
        .write_all(&serde_json::to_vec(&PlanHostResponse { result })?)
        .await?;
    write.write_all(b"\n").await?;
    write.shutdown().await?;
    Ok(())
}

/// Routes `op=execute` through a standalone foreground pVisor process.
pub struct ExecutorRouter {
    host: Arc<PlanHostExecutor>,
    service: PlanHostService,
    pvisor: PVisorProcessClient,
    python: PathBuf,
    supervisor: Option<persisting_agentctl::SupervisorBootstrap>,
}

impl ExecutorRouter {
    /// Worker stack for the product surface (plan + execute only).
    pub fn local_stack(
        pvisor_binary: PathBuf,
        python: PathBuf,
        pythonpath_extra: Vec<PathBuf>,
        plan_script: PathBuf,
        script_args: Vec<String>,
        worker_context: Value,
        supervisor: Option<persisting_agentctl::SupervisorBootstrap>,
    ) -> Result<Self> {
        let host = Arc::new(PlanHostExecutor::new(
            python.clone(),
            pythonpath_extra,
            worker_context,
        ));
        let service = PlanHostService::start(Arc::clone(&host), plan_script, script_args)?;
        Ok(Self {
            host,
            service,
            pvisor: PVisorProcessClient::new(pvisor_binary),
            python,
            supervisor,
        })
    }

    pub async fn run_with_cancel(
        &self,
        task: TaskExpr,
        worker_id: &str,
        cancel: CancellationToken,
        lease_epoch: u64,
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
        let task_id = task.id.clone();
        let started = unix_now();
        let mut spec = task_run_spec(&task, worker_id, lease_epoch);
        spec.supervisor = self.supervisor.clone();
        spec.input = match serde_json::to_value(&task) {
            Ok(value) => value,
            Err(error) => {
                return TaskResult::failure(
                    task_id,
                    format!("encode task as RunSpec input: {error}"),
                    None,
                    worker_id,
                    started,
                );
            }
        };
        let task_path = match self.service.write_task(&task).await {
            Ok(path) => path,
            Err(error) => {
                return TaskResult::failure_with_kind(
                    task_id,
                    format!("prepare pVisor task input failed: {error}"),
                    Some(format!("{error:#}")),
                    worker_id,
                    started,
                    ErrorKind::Infra,
                    true,
                );
            }
        };
        let RunInvocation::Process(process) = &mut spec.invocation;
        process.program = self.python.display().to_string();
        process.args = vec![
            "-u".into(),
            "-c".into(),
            PLAN_HOST_CLIENT.into(),
            self.service.endpoint.clone(),
            self.service.token.clone(),
            task_path.display().to_string(),
            worker_id.into(),
        ];
        process.stdout = StdioMode::Capture;
        process.stderr = StdioMode::Capture;
        let options = PVisorProcessOptions {
            run_home: Some(self.service.task_dir.path().join("runs")),
            run_args: Vec::new(),
        };
        let result = self.pvisor.run(&spec, &options, cancel).await;
        let _ = tokio::fs::remove_file(&task_path).await;
        match result {
            Ok(result) => run_result_to_task_result(result, &task_id, worker_id, started),
            Err(error) => TaskResult::failure_with_kind(
                task_id,
                format!("pVisor Run wait failed: {error}"),
                Some(format!("{error:#}")),
                worker_id,
                started,
                ErrorKind::Infra,
                true,
            ),
        }
    }

    pub async fn shutdown(&self) {
        self.service.stop.cancel();
        self.service.join.abort();
        self.host.shutdown().await;
    }
}

pub(crate) fn task_run_spec(task: &TaskExpr, worker_id: &str, lease_epoch: u64) -> RunSpec {
    let job_id = std::env::var("PERSISTING_PPILOT_JOB_ID").unwrap_or_else(|_| "local".into());
    let run_id = format!(
        "{}{}",
        job_run_id_prefix(&job_id),
        encode_run_id_part(&task.id)
    );
    let mut spec = RunSpec::process(run_id, "ppilot", "ppilot-plan-host");
    spec.lease_epoch = lease_epoch;
    spec.task_id = Some(task.id.clone());
    spec.parent_run_id = Some(persisting_agentctl::RunId::new(format!(
        "ppilot-job-{}",
        encode_run_id_part(&job_id)
    )));
    spec.metadata
        .insert("ppilot.worker_id".into(), Value::String(worker_id.into()));
    spec.metadata
        .insert("ppilot.job_id".into(), Value::String(job_id));
    spec
}

pub(crate) fn job_run_id_prefix(job_id: &str) -> String {
    format!("ppilot-{}-", encode_run_id_part(job_id))
}

fn encode_run_id_part(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(char::from(byte));
            }
            byte => encoded.push_str(&format!("~{byte:02x}")),
        }
    }
    encoded
}

#[cfg(test)]
pub(crate) fn task_result_to_run_result(
    spec: RunSpec,
    attempt_id: persisting_agentctl::AttemptId,
    task: TaskResult,
) -> RunResult {
    let output = ProcessOutput {
        stderr: task.traceback.clone(),
        ..ProcessOutput::default()
    };
    let state = if task.ok {
        RunState::Completed
    } else if task.cancelled {
        RunState::Cancelled
    } else {
        RunState::Failed
    };
    let failure = (!task.ok && !task.cancelled).then(|| RunFailure {
        kind: match task.error_kind {
            Some(ErrorKind::Infra) => RunFailureKind::Infrastructure,
            _ => RunFailureKind::Workload,
        },
        message: task
            .error
            .clone()
            .unwrap_or_else(|| "workload failed".into()),
        retryable: task.retryable,
    });
    let artifacts = task
        .artifacts
        .iter()
        .map(|(name, value)| ArtifactRef {
            name: name.clone(),
            uri: value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| value.to_string()),
            media_type: None,
            digest: None,
        })
        .collect();
    RunResult {
        run_id: spec.run_id,
        attempt_id,
        lease_epoch: spec.lease_epoch,
        state,
        started_at_unix_ms: seconds_to_millis(task.started_at),
        finished_at_unix_ms: seconds_to_millis(task.finished_at),
        exit_code: None,
        failure,
        output,
        value: task.value,
        metrics: task.metrics.into_iter().collect::<BTreeMap<_, _>>(),
        artifacts,
        event_stream_ref: None,
        warnings: Vec::new(),
    }
}

pub(crate) fn run_result_to_task_result(
    result: RunResult,
    task_id: &str,
    worker_id: &str,
    fallback_started: f64,
) -> TaskResult {
    let run_id = result.run_id.as_str().to_string();
    let attempt_id = result.attempt_id.as_str().to_string();
    let lease_epoch = result.lease_epoch;
    if let Some(mut task) = result
        .output
        .stdout
        .as_deref()
        .and_then(|stdout| serde_json::from_str::<TaskResult>(stdout.trim()).ok())
    {
        task.run_id = Some(run_id);
        task.attempt_id = Some(attempt_id);
        task.lease_epoch = lease_epoch;
        return task;
    }
    let started_at = if result.started_at_unix_ms == 0 {
        fallback_started
    } else {
        result.started_at_unix_ms as f64 / 1000.0
    };
    let mut task = match result.state {
        RunState::Completed => {
            let mut task = TaskResult::success(
                task_id,
                result.value.unwrap_or(Value::Null),
                worker_id,
                started_at,
            );
            task.finished_at = Some(result.finished_at_unix_ms as f64 / 1000.0);
            task
        }
        RunState::Cancelled => TaskResult::cancelled(task_id),
        _ => {
            let failure = result.failure.unwrap_or(RunFailure {
                kind: RunFailureKind::Infrastructure,
                message: "pVisor Run failed without a failure record".into(),
                retryable: true,
            });
            TaskResult::failure_with_kind(
                task_id,
                failure.message,
                result.output.stderr,
                worker_id,
                started_at,
                match failure.kind {
                    RunFailureKind::Infrastructure | RunFailureKind::Spawn => ErrorKind::Infra,
                    _ => ErrorKind::Execute,
                },
                failure.retryable,
            )
        }
    };
    task.run_id = Some(run_id);
    task.attempt_id = Some(attempt_id);
    task.lease_epoch = lease_epoch;
    task
}

#[cfg(test)]
fn seconds_to_millis(value: Option<f64>) -> u64 {
    value.unwrap_or_else(unix_now).max(0.0).mul_add(1000.0, 0.0) as u64
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
        let host = PlanHostExecutor::new(PathBuf::from("python3"), vec![], json!({}));
        let cancel = CancellationToken::new();
        let bg = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            bg.cancel();
        });
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 1})).unwrap();
        let t0 = std::time::Instant::now();
        let r = host
            .run_plan_execute(&script, &[], task, "w0", cancel)
            .await;
        assert!(r.cancelled, "{r:?}");
        assert!(t0.elapsed().as_secs_f64() < 2.0);
        host.shutdown().await;
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
        let host = PlanHostExecutor::new(PathBuf::from("python3"), vec![], json!({}));
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 3})).unwrap();
        let r = host
            .run_plan_execute(&script, &[], task, "w0", CancellationToken::new())
            .await;
        assert!(r.ok);
        assert_eq!(r.value, Some(json!({"x2": 6})));
        host.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workload_failure_roundtrips_through_run_result() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fail.py");
        std::fs::write(
            &script,
            r#"
def execute(item):
    raise ValueError("bad item")
"#,
        )
        .unwrap();
        let host = PlanHostExecutor::new(PathBuf::from("python3"), vec![], json!({}));
        let task = TaskExpr::from_value(json!({"id": "bad/task"})).unwrap();
        let result = host
            .run_plan_execute(&script, &[], task, "w0", CancellationToken::new())
            .await;
        assert!(!result.ok);
        assert_eq!(result.error_kind, Some(ErrorKind::Execute));
        assert!(result.error.as_deref().unwrap().contains("bad item"));
        assert!(result.traceback.as_deref().unwrap().contains("ValueError"));
        host.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_context_and_hooks_do_not_change_execute_signature() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("context.py");
        std::fs::write(
            &script,
            r#"
from persisting_ppilot import context

def setup_worker(ctx):
    assert ctx["worker_id"] == "w-context"

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
        let host = PlanHostExecutor::new(
            PathBuf::from("python3"),
            vec![],
            json!({"worker_id": "w-context", "rank": 3}),
        );
        let task = TaskExpr::from_value(json!({"id": "t-0"})).unwrap();
        let result = host
            .run_plan_execute(&script, &[], task, "w-context", CancellationToken::new())
            .await;
        assert!(result.ok);
        assert_eq!(result.metrics.get("rank"), Some(&3.0));
        assert_eq!(result.artifacts.get("worker"), Some(&json!("w-context")));
        host.shutdown().await;
    }
}
