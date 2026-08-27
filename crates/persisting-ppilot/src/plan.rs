//! Run a user plan script and stream typed values (NDJSON on stdout).
//!
//! The control plane never embeds the user's interpreter. It **invokes**
//! `--python` so quirky envs stay isolated; stacks stay in that process.
//!
//! User CLI args after `--` become ``sys.argv`` for the plan module (argparse-friendly).

use crate::task::TaskExpr;
use anyhow::{Context, Result, bail};
use futures::{Stream, StreamExt};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// Bootstrap: set ``sys.argv = [script, *user_args]`` then import plan().
const PLAN_BOOTSTRAP: &str = r#"
import asyncio, json, sys
from pathlib import Path
path = Path(sys.argv[1]).resolve()
user_args = sys.argv[2:]
# So argparse in task.py sees the same argv as `python task.py --foo bar`
sys.argv = [str(path), *user_args]
import importlib.util
spec = importlib.util.spec_from_file_location("user_plan", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

def _dump(item):
    if hasattr(item, "to_dict"):
        item = item.to_dict()
    print(json.dumps(item, ensure_ascii=False), flush=True)

async def _emit():
    if hasattr(mod, "plan"):
        out = mod.plan()
        if asyncio.iscoroutine(out):
            out = await out
        if hasattr(out, "__aiter__"):
            async for item in out:
                _dump(item)
            return
        for item in out:
            _dump(item)
        return
    if hasattr(mod, "PLAN"):
        for item in mod.PLAN:
            _dump(item)
        return
    raise SystemExit("plan script must define plan() or PLAN")

asyncio.run(_emit())
"#;

/// Stream tasks from a plan script under `python`.
pub fn stream_plan_tasks(
    script: PathBuf,
    python: PathBuf,
    script_args: Vec<String>,
) -> Pin<Box<dyn Stream<Item = Result<TaskExpr>> + Send>> {
    Box::pin(
        stream_plan_values(script, python, script_args)
            .map(|value| value.and_then(TaskExpr::from_value)),
    )
}

/// Stream raw JSON values from a Python plan. Consumers apply their own
/// boundary type (`TaskExpr`, production Run, ...), while process isolation,
/// async-generator support, and argument forwarding remain shared.
pub(crate) fn stream_plan_values(
    script: PathBuf,
    python: PathBuf,
    script_args: Vec<String>,
) -> Pin<Box<dyn Stream<Item = Result<serde_json::Value>> + Send>> {
    let (tx, rx) = mpsc::channel::<Result<serde_json::Value>>(64);
    tokio::spawn(async move {
        if let Err(e) = run_plan_process(script, python, script_args, tx.clone()).await {
            let _ = tx.send(Err(e)).await;
        }
    });
    Box::pin(ReceiverStream::new(rx))
}

async fn run_plan_process(
    script: PathBuf,
    python: PathBuf,
    script_args: Vec<String>,
    tx: mpsc::Sender<Result<serde_json::Value>>,
) -> Result<()> {
    let script = script
        .canonicalize()
        .with_context(|| format!("plan script not found: {}", script.display()))?;

    let mut cmd = Command::new(&python);
    cmd.arg("-c")
        .arg(PLAN_BOOTSTRAP)
        .arg(&script)
        .args(&script_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn plan python: {}", python.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing plan stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("missing plan stderr"))?;

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut buf = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            buf.push_str(&line);
            buf.push('\n');
        }
        buf
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parsed = serde_json::from_str::<serde_json::Value>(line)
            .with_context(|| format!("invalid NDJSON from plan: {line}"));
        if tx.send(parsed).await.is_err() {
            break;
        }
    }

    let status = child.wait().await.context("wait plan process")?;
    let err = stderr_task.await.unwrap_or_default();
    if !status.success() {
        bail!(
            "plan script exited {}: {}",
            status.code().unwrap_or(-1),
            err.trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn stream_plan_tasks_emits_flat_items() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("plan.py");
        std::fs::write(
            &script,
            r#"
def plan():
    for i in range(3):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    return item
"#,
        )
        .unwrap();
        let mut stream = stream_plan_tasks(script, PathBuf::from("python3"), vec![]);
        let mut ids = Vec::new();
        while let Some(item) = stream.next().await {
            ids.push(item.unwrap().id);
        }
        assert_eq!(ids, vec!["t-0", "t-1", "t-2"]);
    }

    #[tokio::test]
    async fn stream_plan_values_supports_async_generators_and_forwards_args() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("production.py");
        std::fs::write(
            &script,
            r#"
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, required=True)
args = parser.parse_args()

async def plan():
    for i in range(args.count):
        yield {"id": f"run-{i}", "command": ["/bin/true"]}
"#,
        )
        .unwrap();
        let values = stream_plan_values(
            script,
            PathBuf::from("python3"),
            vec!["--count".into(), "2".into()],
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].as_ref().unwrap()["id"], "run-0");
        assert_eq!(values[1].as_ref().unwrap()["command"][0], "/bin/true");
    }

    #[tokio::test]
    async fn stream_plan_values_surfaces_python_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("broken.py");
        std::fs::write(
            &script,
            r#"
def plan():
    raise RuntimeError("planner exploded")
"#,
        )
        .unwrap();
        let errors = stream_plan_values(script, PathBuf::from("python3"), vec![])
            .collect::<Vec<_>>()
            .await;
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("planner exploded")
        );
    }
}
