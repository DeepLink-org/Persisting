//! pVisor executor provider used by pPilot workers while the Driver schedules TaskExpr.
//!
//! Every TaskExpr is adapted to one stable RunSpec. The long-lived Python host
//! implements [`RunExecutor`], so execution, cancellation and terminal state all
//! pass through pVisor without paying one Python import per task.
//!
//! **Primitive:** [`Executor`] trait · [`ExecutorRouter`] (product: `op=execute` only).
//!
//! ```text
//! Driver --ask--> WorkerActor -- RunSpec --> pVisor --> plan.py::execute(item)
//! ```

use crate::agent_abi::{AgentAbiClient, AgentAbiClientConfig};
use crate::python_env;
use crate::runtime_bridge::PilotRuntimeBridge;
use crate::task::{unix_now, ErrorKind, TaskExpr, TaskResult};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use persisting_control::{
    ArtifactRef, ExecutorDescriptor, ExecutorKind, IsolationKind, ProcessOutput, RunFailure,
    RunFailureKind, RunInvocation, RunResult, RunSpec, RunState,
};
use persisting_pvisor::{
    AgentClientRole, AgentEffectOutcome, AgentProcessRegistration, AttemptContext, PVisor,
    RunExecutor,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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

#[async_trait]
impl RunExecutor for PlanExecuteExecutor {
    fn descriptor(&self) -> ExecutorDescriptor {
        ExecutorDescriptor {
            name: "ppilot-plan-host-v1".into(),
            kind: ExecutorKind::Process,
            isolation: IsolationKind::HostProcess,
            enforces_capabilities: false,
            supports_checkpoint: true,
            supports_migration: false,
        }
    }

    fn supports(&self, invocation: &RunInvocation) -> bool {
        matches!(invocation, RunInvocation::Process(_))
    }

    async fn execute(&self, context: AttemptContext) -> RunResult {
        let spec = context.spec().clone();
        let started_at_unix_ms = unix_ms();
        context
            .transition(RunState::Starting, Some("starting pPilot plan host".into()))
            .await;
        let task = match serde_json::from_value::<TaskExpr>(spec.input.clone()) {
            Ok(task) => task,
            Err(error) => {
                return failed_run_result(
                    &spec,
                    &context,
                    started_at_unix_ms,
                    RunFailureKind::InvalidSpec,
                    format!("decode pPilot TaskExpr from RunSpec.input: {error}"),
                    false,
                );
            }
        };
        let worker_id = spec
            .metadata
            .get("ppilot.worker_id")
            .and_then(Value::as_str)
            .unwrap_or("ppilot-worker")
            .to_string();
        let agent_abi = match connect_agent_abi(&spec, &context, &worker_id) {
            Ok(client) => client,
            Err(error) => {
                return failed_run_result(
                    &spec,
                    &context,
                    started_at_unix_ms,
                    RunFailureKind::Infrastructure,
                    format!("connect pPilot to pVisor Agent ABI: {error:#}"),
                    true,
                );
            }
        };
        context.transition(RunState::Running, None).await;
        let effect_id = format!("task:{}", task.id);
        if let Some(bridge) = agent_abi.as_ref() {
            let digest = format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&task).unwrap_or_default())
            );
            let job_id = spec
                .metadata
                .get("ppilot.job_id")
                .and_then(Value::as_str)
                .unwrap_or("local");
            if let Err(error) = bridge.begin_effect(
                &effect_id,
                "ppilot.task",
                digest,
                Some(format!("{job_id}/{}", task.id)),
            ) {
                return failed_run_result(
                    &spec,
                    &context,
                    started_at_unix_ms,
                    RunFailureKind::Infrastructure,
                    format!("begin pPilot task effect: {error:#}"),
                    true,
                );
            }
        }
        let task_result = self
            .run_with_cancel(task, &worker_id, context.cancellation())
            .await;
        let effect_outcome = if task_result.ok {
            AgentEffectOutcome::Committed
        } else if task_result.cancelled {
            AgentEffectOutcome::Aborted
        } else {
            AgentEffectOutcome::Unknown
        };
        let mut result = task_result_to_run_result(spec, context.attempt_id().clone(), task_result);
        if let Some(bridge) = agent_abi {
            if let Err(error) = bridge.complete_effect(&effect_id, effect_outcome) {
                result.state = RunState::Failed;
                result.exit_code = None;
                result.failure = Some(RunFailure {
                    kind: RunFailureKind::Infrastructure,
                    message: format!("complete pPilot task effect: {error:#}"),
                    retryable: true,
                });
            }
            result.warnings.extend(bridge.finish().await);
        }
        result
    }
}

fn connect_agent_abi(
    spec: &RunSpec,
    context: &AttemptContext,
    worker_id: &str,
) -> Result<Option<PilotRuntimeBridge>> {
    let RunInvocation::Process(process) = &spec.invocation;
    let Some(config) = AgentAbiClientConfig::from_environment(
        &process.env,
        format!("{worker_id}:{}", context.attempt_id()),
        AgentClientRole::Pilot,
        spec.agent.name.clone(),
    )?
    else {
        return Ok(None);
    };
    Ok(Some(PilotRuntimeBridge::start(
        AgentAbiClient::new(config),
        AgentProcessRegistration {
            pid: std::process::id(),
            role: "ppilot-worker".into(),
            executable: std::env::current_exe()
                .ok()
                .map(|path| path.display().to_string()),
        },
        context.cancellation(),
    )?))
}

/// Routes `op=execute` to [`PlanExecuteExecutor`]. Unknown ops fail clearly.
pub struct ExecutorRouter {
    host: Arc<PlanHostExecutor>,
    pvisor: PVisor,
    supervisor: Option<persisting_control::SupervisorBootstrap>,
}

impl ExecutorRouter {
    /// Worker stack for the product surface (plan + execute only).
    pub fn local_stack(
        python: PathBuf,
        pythonpath_extra: Vec<PathBuf>,
        plan_script: PathBuf,
        script_args: Vec<String>,
        worker_context: Value,
        supervisor: Option<persisting_control::SupervisorBootstrap>,
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
        let pvisor = PVisor::builder()
            .executors(vec![Arc::clone(&execute) as Arc<dyn RunExecutor>])
            .build();
        Self {
            host,
            pvisor,
            supervisor,
        }
    }

    #[cfg(test)]
    async fn run(&self, task: TaskExpr, worker_id: &str) -> TaskResult {
        self.run_with_cancel(task, worker_id, CancellationToken::new(), 1)
            .await
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
        let handle = match self.pvisor.submit(spec).await {
            Ok(handle) => handle,
            Err(error) => {
                return TaskResult::failure_with_kind(
                    task_id,
                    format!("pVisor submit failed: {error}"),
                    Some(format!("{error:#}")),
                    worker_id,
                    started,
                    ErrorKind::Infra,
                    true,
                );
            }
        };
        let cancellation = handle.cancellation();
        let wait = handle.wait();
        tokio::pin!(wait);
        let result = tokio::select! {
            result = &mut wait => result,
            _ = cancel.cancelled() => {
                cancellation.cancel();
                wait.await
            }
        };
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
        self.host.shutdown().await;
    }
}

pub(crate) fn task_run_spec(task: &TaskExpr, worker_id: &str, lease_epoch: u64) -> RunSpec {
    let job_id = std::env::var("PERSISTING_COMPUTE_JOB_ID").unwrap_or_else(|_| "local".into());
    let run_id = format!(
        "{}{}",
        job_run_id_prefix(&job_id),
        encode_run_id_part(&task.id)
    );
    let mut spec = RunSpec::process(run_id, "ppilot", "ppilot-plan-host");
    spec.lease_epoch = lease_epoch;
    spec.task_id = Some(task.id.clone());
    spec.parent_run_id = Some(persisting_control::RunId::new(format!(
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

pub(crate) fn task_result_to_run_result(
    spec: RunSpec,
    attempt_id: persisting_control::AttemptId,
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

fn failed_run_result(
    spec: &RunSpec,
    context: &AttemptContext,
    started_at_unix_ms: u64,
    kind: RunFailureKind,
    message: String,
    retryable: bool,
) -> RunResult {
    RunResult {
        run_id: spec.run_id.clone(),
        attempt_id: context.attempt_id().clone(),
        lease_epoch: spec.lease_epoch,
        state: RunState::Failed,
        started_at_unix_ms,
        finished_at_unix_ms: unix_ms(),
        exit_code: None,
        failure: Some(RunFailure {
            kind,
            message,
            retryable,
        }),
        output: ProcessOutput::default(),
        value: None,
        metrics: BTreeMap::new(),
        artifacts: Vec::new(),
        event_stream_ref: None,
        warnings: Vec::new(),
    }
}

fn seconds_to_millis(value: Option<f64>) -> u64 {
    value.unwrap_or_else(unix_now).max(0.0).mul_add(1000.0, 0.0) as u64
}

fn unix_ms() -> u64 {
    seconds_to_millis(Some(unix_now()))
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
            None,
        );
        let cancel = CancellationToken::new();
        let bg = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            bg.cancel();
        });
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 1})).unwrap();
        let t0 = std::time::Instant::now();
        let r = router.run_with_cancel(task, "w0", cancel, 1).await;
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
            None,
        );
        let task = TaskExpr::from_value(json!({"id": "t-0", "x": 3})).unwrap();
        let r = router.run(task, "w0").await;
        assert!(r.ok);
        assert_eq!(r.run_id.as_deref(), Some("ppilot-local-t-0"));
        assert_eq!(r.value, Some(json!({"x2": 6})));
        router.shutdown().await;
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
        let router = ExecutorRouter::local_stack(
            PathBuf::from("python3"),
            vec![],
            script,
            vec![],
            json!({}),
            None,
        );
        let task = TaskExpr::from_value(json!({"id": "bad/task"})).unwrap();
        let result = router.run(task, "w0").await;
        assert!(!result.ok);
        assert_eq!(result.run_id.as_deref(), Some("ppilot-local-bad~2ftask"));
        assert_eq!(result.error_kind, Some(ErrorKind::Execute));
        assert!(result.error.as_deref().unwrap().contains("bad item"));
        assert!(result.traceback.as_deref().unwrap().contains("ValueError"));
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
            None,
        );
        let task = TaskExpr::from_value(json!({"id": "t-0"})).unwrap();
        let result = router.run(task, "w-context").await;
        assert!(result.ok);
        assert_eq!(result.metrics.get("rank"), Some(&3.0));
        assert_eq!(result.artifacts.get("worker"), Some(&json!("w-context")));
        router.shutdown().await;
    }
}
