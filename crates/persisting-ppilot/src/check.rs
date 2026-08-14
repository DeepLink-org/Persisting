//! Local validation: prove env + plan + execute before scale-out.
//!
//! The embedding host exposes this through [`crate::cli::PPilotArgs::check`].

use crate::plan::stream_plan_tasks;
use crate::python_env::{self, pythonpath_for_script};
use crate::runtime::{run_local_fleet, RunOptions};
use crate::task::TaskExpr;
use anyhow::{bail, Context, Result};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct CheckOptions {
    pub script: PathBuf,
    pub python: PathBuf,
    /// Max tasks to actually execute (0 = all).
    pub limit: usize,
    pub workers: usize,
    pub verbose: bool,
    pub pythonpath_extra: Vec<PathBuf>,
    /// Forwarded to `task.py` as `sys.argv[1:]` (after `--`).
    pub script_args: Vec<String>,
}

#[derive(Debug, Default)]
pub struct CheckReport {
    pub python_ok: bool,
    pub python_version: Option<String>,
    pub plan_tasks: usize,
    pub plan_ops: BTreeMap<String, usize>,
    pub execute_ok: bool,
    pub run_ok: usize,
    pub run_fail: usize,
    pub errors: Vec<String>,
}

impl CheckReport {
    pub fn passed(&self) -> bool {
        self.python_ok
            && self.errors.is_empty()
            && self.plan_tasks > 0
            && self.execute_ok
            && self.run_fail == 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "ok": self.passed(),
            "python": { "ok": self.python_ok, "version": self.python_version },
            "plan": { "tasks": self.plan_tasks, "ops": self.plan_ops },
            "execute": { "ok": self.execute_ok },
            "run": { "ok": self.run_ok, "failed": self.run_fail },
            "errors": self.errors,
        })
    }
}

/// Full local check pipeline. Progress → stderr; JSON summary → stdout.
pub async fn run_check(opts: CheckOptions) -> Result<CheckReport> {
    let mut report = CheckReport::default();
    let mut extras = pythonpath_for_script(&opts.script);
    extras.extend(opts.pythonpath_extra.iter().cloned());

    eprint_stage(1, 4, "python env");
    match probe_python(&opts.python).await {
        Ok(ver) => {
            report.python_ok = true;
            report.python_version = Some(ver.clone());
            eprintln!("  OK  {} ({})", opts.python.display(), ver.trim());
        }
        Err(e) => {
            report.python_ok = false;
            report.errors.push(format!("python: {e:#}"));
            eprintln!("  FAIL  {e:#}");
            print_summary(&report);
            return Ok(report);
        }
    }
    if let Some(pp) = python_env::merge_pythonpath(&extras) {
        eprintln!("  PYTHONPATH+= {}", shorten_pp(&pp));
    }

    eprint_stage(2, 4, "plan emit + schema");
    let tasks =
        match collect_plan_tasks(&opts.script, &opts.python, &extras, &opts.script_args).await {
            Ok(t) => t,
            Err(e) => {
                report.errors.push(format!("plan: {e:#}"));
                eprintln!("  FAIL  {e:#}");
                print_summary(&report);
                return Ok(report);
            }
        };
    if tasks.is_empty() {
        report.errors.push("plan emitted zero tasks".into());
        eprintln!("  FAIL  no tasks");
        print_summary(&report);
        return Ok(report);
    }
    report.plan_tasks = tasks.len();
    for t in &tasks {
        *report.plan_ops.entry(t.op.clone()).or_default() += 1;
    }
    eprintln!(
        "  OK  {} task(s)  ops={}",
        tasks.len(),
        format_ops(&report.plan_ops)
    );
    if opts.verbose {
        for t in tasks.iter().take(5) {
            eprintln!("    {}", t.to_ndjson().unwrap_or_default());
        }
        if tasks.len() > 5 {
            eprintln!("    … {} more", tasks.len() - 5);
        }
    }

    eprint_stage(3, 4, "resolve execute");
    match probe_plan_execute(&opts.python, &opts.script, &extras).await {
        Ok(()) => {
            report.execute_ok = true;
            eprintln!("  OK  {}::execute(item)", opts.script.display());
        }
        Err(e) => {
            report.execute_ok = false;
            report.errors.push(format!("execute: {e:#}"));
            eprintln!("  FAIL  execute: {e:#}");
            print_summary(&report);
            return Ok(report);
        }
    }

    eprint_stage(4, 4, "local run");
    let to_run = if opts.limit == 0 {
        tasks.len()
    } else {
        opts.limit.min(tasks.len())
    };
    // Skip the remainder so --limit actually bounds execute without changing plan().
    let skip = if to_run < tasks.len() {
        eprintln!(
            "  note: --limit {to_run} (skipping {} of {} plan tasks)",
            tasks.len() - to_run,
            tasks.len()
        );
        tasks[to_run..].iter().map(|t| t.id.clone()).collect()
    } else {
        crate::skip::SkipSet::new()
    };

    let run_opts = RunOptions {
        script: opts.script.clone(),
        python: opts.python.clone(),
        workers: opts.workers.max(1),
        max_inflight: opts.workers.max(1),
        per_worker_inflight: 1,
        pythonpath_extra: extras.clone(),
        script_args: opts.script_args.clone(),
        infra_retries: 2,
        job_cancel: tokio_util::sync::CancellationToken::new(),
        observer: crate::observe::Observer::disabled(),
        skip_task_ids: skip,
        checkpoint: None,
        sink_submitter: None,
        coordinator: None,
    };

    match run_local_fleet(run_opts, |_| {}).await {
        Ok(results) => {
            for r in &results {
                if r.ok {
                    report.run_ok += 1;
                } else {
                    report.run_fail += 1;
                    let msg = format!(
                        "task {}: {}",
                        r.task_id,
                        r.error.clone().unwrap_or_else(|| "failed".into())
                    );
                    report.errors.push(msg.clone());
                    if opts.verbose {
                        if let Some(tb) = &r.traceback {
                            eprintln!("  FAIL  {msg}\n{tb}");
                        } else {
                            eprintln!("  FAIL  {msg}");
                        }
                    }
                }
            }
            eprintln!(
                "  {}  {}/{} ok",
                if report.run_fail == 0 { "OK" } else { "FAIL" },
                report.run_ok,
                results.len()
            );
        }
        Err(e) => {
            report.errors.push(format!("run: {e:#}"));
            eprintln!("  FAIL  {e:#}");
        }
    }

    print_summary(&report);
    Ok(report)
}

fn eprint_stage(n: u32, total: u32, title: &str) {
    eprintln!("[{n}/{total}] {title}");
}

fn print_summary(report: &CheckReport) {
    println!("{}", report.to_json());
}

fn format_ops(ops: &BTreeMap<String, usize>) -> String {
    ops.iter()
        .map(|(k, v)| format!("{k}×{v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn shorten_pp(pp: &str) -> String {
    let parts: Vec<String> = std::env::split_paths(pp)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if parts.len() <= 3 {
        parts.join(":")
    } else {
        format!("{} … (+{} paths)", parts[..2].join(":"), parts.len() - 2)
    }
}

async fn probe_python(python: &Path) -> Result<String> {
    let out = Command::new(python)
        .args(["-c", "import sys; print(sys.version.split()[0])"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .with_context(|| format!("exec {}", python.display()))?;
    if !out.status.success() {
        bail!(
            "exit {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn collect_plan_tasks(
    script: &Path,
    python: &Path,
    extras: &[PathBuf],
    script_args: &[String],
) -> Result<Vec<TaskExpr>> {
    if let Some(pp) = python_env::merge_pythonpath(extras) {
        std::env::set_var("PYTHONPATH", pp);
    }
    let mut stream = stream_plan_tasks(
        script.to_path_buf(),
        python.to_path_buf(),
        script_args.to_vec(),
    );
    let mut tasks = Vec::new();
    while let Some(item) = stream.next().await {
        tasks.push(item?);
    }
    Ok(tasks)
}

async fn probe_plan_execute(python: &Path, script: &Path, extras: &[PathBuf]) -> Result<()> {
    let script = script
        .canonicalize()
        .with_context(|| format!("plan script {}", script.display()))?;
    let code = r#"
import importlib.util, sys
from pathlib import Path
path = Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("user_plan_probe", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
if not hasattr(mod, "execute") or not callable(mod.execute):
    raise SystemExit("plan must define execute(item)")
print("ok", flush=True)
"#;
    let mut cmd = Command::new(python);
    cmd.arg("-c")
        .arg(code)
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    python_env::apply_pythonpath(&mut cmd, extras);
    let out = cmd.output().await.context("probe execute")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
