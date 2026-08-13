//! Embeddable argument surface for a pPilot host.
//!
//! With a script, `--check` validates first and the default path executes it.

use crate::check::{run_check, CheckOptions};
use crate::checkpoint::{CheckpointLedger, CheckpointTracker};
use crate::coordination::RunCoordinator;
use crate::observe::{Observer, ObserverOptions};
use crate::runtime::{run_fleet, RunOptions};
use crate::sink::{JsonlFileSink, ResultSink, TeeSink};
use crate::sink_writer::spawn_coordinated_sink_writer;
use crate::skip::SkipSet;
use crate::task::TaskResult;
use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Arguments accepted by an embedding pPilot host.
#[derive(Debug, Clone, Args)]
#[command(
    about = "Run a pPilot plan (`plan()` + `execute(item)`).",
    long_about = "pPilot — Durable Run Orchestrator.\n\nRun a Phase-1 map-style plan with bounded concurrency, checkpoint/resume, infrastructure retry, and a single result sink."
)]
pub struct PPilotArgs {
    /// Plan script (`plan()` / `execute()`).
    #[arg(value_name = "SCRIPT")]
    pub script: PathBuf,

    /// Validate env + plan + execute (+ sample run) instead of a full run.
    #[arg(long)]
    pub check: bool,

    #[arg(short = 'w', long, default_value_t = 4)]
    pub workers: usize,

    /// Concurrent Execute slots per logical worker/rank. Each slot is its own
    /// WorkerActor + Python host (real parallelism). Default 1 — best when task
    /// times vary widely across workers.
    #[arg(long, default_value_t = 1)]
    pub per_worker: usize,

    /// Global inflight cap (default: workers × per-worker, or WORLD_SIZE × per-worker under torchrun).
    #[arg(long)]
    pub max_inflight: Option<usize>,

    /// Infrastructure retries when a worker ask fails (not semantic retry).
    #[arg(long, default_value_t = 2)]
    pub retries: u32,

    /// Durable unique sink directory (`ready.ndjson` + `failures.ndjson` + `checkpoint.json`).
    #[arg(long, value_name = "DIR")]
    pub sink: Option<PathBuf>,

    /// pChronicle Run control root. Supports local paths and object-store URIs.
    /// Defaults to `--sink`, keeping `run-control/` beside the result journal.
    #[arg(long, env = "PERSISTING_PPILOT_CONTROL_URI", value_name = "URI")]
    pub control_uri: Option<String>,

    /// Lifetime advertised by each newly issued Run lease (minimum: 1000ms).
    #[arg(long, default_value_t = 30_000)]
    pub lease_ttl_ms: u64,

    /// Logical run identifier exposed to workers through `persisting_ppilot.context()`.
    /// Defaults to the sink directory name, or the plan filename for ephemeral runs.
    #[arg(long)]
    pub job_id: Option<String>,

    /// Capability labels exposed as `context()["labels"]` (comma-separated).
    /// They are informational in this release; scheduling remains least-loaded.
    #[arg(long, value_delimiter = ',')]
    pub worker_label: Vec<String>,

    /// Resume from `--sink`: skip task ids already in ready/failures.
    #[arg(long)]
    pub resume: bool,

    /// With `--resume`, run failures of these kinds again (`execute`, `infra`,
    /// or `cancelled`). May be repeated or comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub rerun_failed: Vec<String>,

    /// Also append terminal results to a Lance trajectory (requires `--sink`).
    /// Writes `ppilot.result` / `ppilot.failure` events under traj storage.
    #[cfg(feature = "traj-sink")]
    #[arg(long)]
    pub traj: bool,

    /// Lance storage root (default: `{sink}/traj`).
    #[cfg(feature = "traj-sink")]
    #[arg(long, value_name = "DIR")]
    pub traj_storage: Option<PathBuf>,

    /// Trajectory agent_id (default: `ppilot`).
    #[cfg(feature = "traj-sink")]
    #[arg(long, default_value = "ppilot")]
    pub traj_agent: String,

    /// Trajectory session_id (default: sink directory name).
    #[cfg(feature = "traj-sink")]
    #[arg(long)]
    pub traj_session: Option<String>,

    #[arg(long, env = "PERSISTING_PYTHON", default_value = "python3")]
    pub python: PathBuf,

    /// Extra PYTHONPATH entries (plan dir is always added).
    #[arg(short = 'E', long = "pythonpath")]
    pub pythonpath: Vec<PathBuf>,

    /// Result stream on stdout. Default: `ndjson`; with `--observe` default becomes `quiet`
    /// so progress is not drowned out (pass `--results ndjson` to keep both).
    #[arg(long, value_enum)]
    pub results: Option<ResultsFormat>,

    /// With `--check`: max tasks to execute (0 = all). Ignored on normal run.
    #[arg(long, default_value_t = 0)]
    pub limit: usize,

    /// Live queue / placement / duration progress on stderr (`[obs] …`).
    /// Must be **before** `--`. Env: `PERSISTING_OBSERVE=1`.
    #[arg(long, env = "PERSISTING_OBSERVE")]
    pub observe: bool,

    /// Append observe events as NDJSON to FILE (implies observe).
    #[arg(long, value_name = "FILE", env = "PERSISTING_OBSERVE_FILE")]
    pub observe_file: Option<PathBuf>,

    /// Also emit observe NDJSON on stderr (in addition to `[obs]` human lines).
    #[arg(long)]
    pub observe_json: bool,

    /// More stderr tracing (`persisting_ppilot` + Pulsing at info).
    #[arg(short, long)]
    pub verbose: bool,

    /// Args forwarded to the plan script (`sys.argv[1:]`). Put after `--`.
    /// Example forwarded values: `--model x --n 2`.
    #[arg(last = true, value_name = "SCRIPT_ARGS")]
    pub script_args: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ResultsFormat {
    Ndjson,
    Summary,
    Quiet,
}

/// Run pPilot (async). Caller owns the Tokio runtime.
pub async fn run_ppilot(args: PPilotArgs) -> Result<ExitCode> {
    let script = args.script;

    if args.check {
        let report = run_check(CheckOptions {
            script,
            python: args.python,
            limit: args.limit,
            workers: args.workers.max(1),
            verbose: args.verbose,
            pythonpath_extra: args.pythonpath,
            script_args: args.script_args,
        })
        .await?;
        return Ok(if report.passed() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let under_torch = std::env::var_os("RANK").is_some();
    let job_id = args.job_id.clone().unwrap_or_else(|| {
        args.sink
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or_else(|| {
                script
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "ppilot".into())
    });
    std::env::set_var("PERSISTING_PPILOT_JOB_ID", &job_id);
    if let Some(dir) = &args.sink {
        std::env::set_var("PERSISTING_PPILOT_OUTPUT_DIR", dir);
    }
    if !args.worker_label.is_empty() {
        std::env::set_var(
            "PERSISTING_PPILOT_WORKER_LABELS",
            args.worker_label.join(","),
        );
    }
    let per_worker = args.per_worker.max(1);
    let max_inflight = args.max_inflight.unwrap_or_else(|| {
        if under_torch {
            let ws: usize = std::env::var("WORLD_SIZE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4);
            ws.saturating_mul(per_worker).max(1)
        } else {
            args.workers.saturating_mul(per_worker).max(1)
        }
    });

    let job_cancel = CancellationToken::new();
    let cancel_bg = job_cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::warn!("Ctrl-C: cancelling job (RunFutures)");
        cancel_bg.cancel();
    });

    let file_sink: Option<Arc<dyn ResultSink>> = if let Some(dir) = &args.sink {
        let mut sinks: Vec<Box<dyn ResultSink>> = Vec::new();
        sinks.push(Box::new(
            JsonlFileSink::open(dir).await.context("open jsonl sink")?,
        ));
        #[cfg(feature = "traj-sink")]
        if args.traj {
            let traj_storage = args
                .traj_storage
                .clone()
                .unwrap_or_else(|| dir.join("traj"));
            tokio::fs::create_dir_all(&traj_storage)
                .await
                .with_context(|| format!("mkdir traj {}", traj_storage.display()))?;
            let session = args.traj_session.clone().unwrap_or_else(|| {
                dir.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("run")
                    .to_string()
            });
            let lance = crate::sink_traj::LanceResultSink::new(
                traj_storage.display().to_string(),
                args.traj_agent.clone(),
                session.clone(),
            );
            if let Ok(ledger) = CheckpointLedger::load(dir).await {
                lance.seed_seen(ledger.skip_ids());
            }
            eprintln!(
                "[traj] lance append → {}/{}/{}",
                traj_storage.display(),
                args.traj_agent,
                session
            );
            sinks.push(Box::new(lance));
        }
        Some(Arc::new(TeeSink::new(sinks)))
    } else {
        None
    };

    if args.resume && file_sink.is_none() {
        bail!("--resume requires --sink DIR");
    }
    if args.control_uri.is_some() && file_sink.is_none() {
        bail!("--control-uri requires --sink DIR for the durable result journal");
    }
    if !args.rerun_failed.is_empty() && !args.resume {
        bail!("--rerun-failed requires --resume --sink DIR");
    }
    for kind in &args.rerun_failed {
        if !matches!(kind.as_str(), "execute" | "infra" | "cancelled") {
            bail!("--rerun-failed expects execute, infra, or cancelled (got {kind:?})");
        }
    }

    #[cfg(feature = "traj-sink")]
    if args.traj && args.sink.is_none() {
        bail!("--traj requires --sink DIR (JSONL ledger for --resume)");
    }

    let (skip_task_ids, checkpoint) = if let Some(dir) = &args.sink {
        let tracker = Arc::new(CheckpointTracker::new(dir.clone()));
        if args.resume {
            let ledger = CheckpointLedger::load(dir)
                .await
                .context("load checkpoint ledger")?;
            let mut skip = ledger.skip_ids();
            let rerun = ledger
                .failed_ids_matching(dir, &args.rerun_failed)
                .await
                .context("filter failed task ids")?;
            for id in &rerun {
                skip.remove(id);
            }
            eprintln!(
                "[ckpt] resume: ready={} fail={} rerun={} skip_total={}",
                ledger.ready.len(),
                ledger.failed.len(),
                rerun.len(),
                skip.len()
            );
            tracker.seed_from_ledger(&ledger);
            (skip.into_iter().collect(), Some(tracker))
        } else {
            (SkipSet::new(), Some(tracker))
        }
    } else {
        (SkipSet::new(), None)
    };

    let coordinator = if let (Some(dir), Some(sink)) = (&args.sink, &file_sink) {
        let control_root = args
            .control_uri
            .clone()
            .unwrap_or_else(|| dir.display().to_string());
        let coordinator = Arc::new(
            RunCoordinator::open_for_job(&control_root, dir, args.lease_ttl_ms, &job_id)
                .await
                .context("open pPilot Run coordinator")?,
        );
        let observer = coordinator.durable_attempt_observer();
        let report = coordinator
            .reconcile(sink.as_ref(), &observer)
            .await
            .context("reconcile pPilot Runs")?;
        for task_id in &report.committed_task_ids {
            skip_task_ids.insert(task_id.clone());
        }
        for task_id in &report.retry_task_ids {
            skip_task_ids.remove(task_id);
        }
        for task_id in &report.deferred_task_ids {
            skip_task_ids.insert(task_id.clone());
        }
        if report.recovered_commits > 0
            || report.recovered_sink_appends > 0
            || report.fenced_results > 0
            || !report.retry_task_ids.is_empty()
        {
            eprintln!(
                "[reconcile] commits={} sink={} fenced={} active={} retry={}",
                report.recovered_commits,
                report.recovered_sink_appends,
                report.fenced_results,
                report.active_attempts,
                report.retry_task_ids.len()
            );
        }
        Some(coordinator)
    } else {
        None
    };

    let observe_on = args.observe || args.observe_file.is_some() || args.observe_json;
    let observer = if observe_on {
        Observer::open(ObserverOptions {
            human: true,
            json_stderr: args.observe_json,
            path: args.observe_file.clone(),
        })
        .await
        .context("open observe sink")?
    } else {
        Observer::disabled()
    };

    // With observe, default to quiet results so `[obs]` lines are visible.
    let results_fmt = args.results.unwrap_or(if observe_on {
        ResultsFormat::Quiet
    } else {
        ResultsFormat::Ndjson
    });

    let sink_writer = file_sink.as_ref().map(|sink| {
        spawn_coordinated_sink_writer(
            Arc::clone(sink),
            checkpoint.clone(),
            Some(skip_task_ids.clone()),
            max_inflight.saturating_mul(2).max(16),
            Arc::clone(coordinator.as_ref().expect("sink has coordinator")),
        )
    });
    let sink_submit = sink_writer.as_ref().map(|w| w.submitter());

    let opts = RunOptions {
        script,
        python: args.python,
        workers: args.workers,
        max_inflight,
        per_worker_inflight: per_worker,
        pythonpath_extra: args.pythonpath,
        script_args: args.script_args,
        infra_retries: args.retries,
        job_cancel,
        observer,
        skip_task_ids,
        checkpoint: checkpoint.clone(),
        sink_submitter: sink_submit,
        coordinator,
    };

    let collected = run_fleet(opts, move |r: TaskResult| {
        if matches!(results_fmt, ResultsFormat::Ndjson) {
            if let Ok(line) = r.to_ndjson() {
                println!("{line}");
            }
        }
    })
    .await
    .context("run fleet")?;

    if let Some(w) = sink_writer {
        w.join().await.context("sink persist")?;
    }

    if let Some(ckpt) = &checkpoint {
        let _ = ckpt.flush().await;
        eprintln!("{}", ckpt.summary_line());
    }

    if collected.is_empty() && under_torch {
        let rank: usize = std::env::var("RANK")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if rank != 0 {
            return Ok(ExitCode::SUCCESS);
        }
    }

    let failed = collected.iter().filter(|r| !r.ok || r.cancelled).count();
    let summary = build_run_summary(&collected, args.sink.as_ref());
    if let Some(dir) = &args.sink {
        tokio::fs::write(
            dir.join("summary.json"),
            serde_json::to_vec_pretty(&summary).context("encode run summary")?,
        )
        .await
        .context("write summary.json")?;
    }

    if matches!(results_fmt, ResultsFormat::Summary) {
        println!("{summary}");
        for r in &collected {
            if !r.ok {
                if let Ok(line) = r.to_ndjson() {
                    eprintln!("{line}");
                }
            }
        }
    }

    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn build_run_summary(results: &[TaskResult], sink: Option<&PathBuf>) -> serde_json::Value {
    let mut aggregates: BTreeMap<String, (f64, u64)> = BTreeMap::new();
    let mut error_kinds: BTreeMap<String, u64> = BTreeMap::new();
    let mut artifacts = 0u64;
    for result in results {
        for (name, value) in &result.metrics {
            let entry = aggregates.entry(name.clone()).or_insert((0.0, 0));
            entry.0 += value;
            entry.1 += 1;
        }
        artifacts += result.artifacts.len() as u64;
        if let Some(kind) = &result.error_kind {
            *error_kinds
                .entry(format!("{kind:?}").to_lowercase())
                .or_default() += 1;
        }
    }
    let metrics: BTreeMap<_, _> = aggregates
        .into_iter()
        .map(|(name, (sum, count))| {
            (
                name,
                serde_json::json!({"count": count, "sum": sum, "mean": sum / count as f64}),
            )
        })
        .collect();
    serde_json::json!({
        "total": results.len(),
        "ok": results.iter().filter(|r| r.ok && !r.cancelled).count(),
        "failed": results.iter().filter(|r| !r.ok || r.cancelled).count(),
        "cancelled": results.iter().filter(|r| r.cancelled).count(),
        "error_kinds": error_kinds,
        "metrics": metrics,
        "artifact_count": artifacts,
        "sink": sink.map(|p| p.display().to_string()),
    })
}

/// Ensure tracing is initialized once (safe to call from nested CLI).
///
/// Default is quiet: hush Pulsing actor lifecycle noise. Override with `RUST_LOG`,
/// or pass `--verbose` for `persisting_ppilot=info,pulsing_actor=info`.
pub fn init_tracing() {
    init_tracing_with_verbose(false);
}

/// Same as [`init_tracing`], with optional verbose default when `RUST_LOG` is unset.
pub fn init_tracing_with_verbose(verbose: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                if verbose {
                    tracing_subscriber::EnvFilter::new(
                        "persisting_ppilot=info,pulsing_actor=info,info",
                    )
                } else {
                    // Quiet default: only warn+ from Pulsing; pPilot stays at warn.
                    tracing_subscriber::EnvFilter::new(
                        "persisting_ppilot=warn,pulsing_actor=warn,warn",
                    )
                }
            }),
        )
        .with_writer(std::io::stderr)
        .with_target(verbose)
        .try_init();
}
