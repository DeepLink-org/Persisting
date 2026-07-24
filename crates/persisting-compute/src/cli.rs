//! CLI surface for compute — used by `persisting compute [plan]`.
//!
//! - With script → **run** (default); `--check` validates first
//! - `--self-test` → built-in smoke (no user plan)

use crate::check::{run_check, run_self_test, CheckOptions};
use crate::checkpoint::{CheckpointLedger, CheckpointTracker};
use crate::observe::{Observer, ObserverOptions};
use crate::runtime::{run_fleet, RunOptions};
use crate::sink::{JsonlFileSink, ResultSink, TeeSink};
use crate::sink_writer::spawn_sink_writer;
use crate::skip::SkipSet;
use crate::task::TaskResult;
use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// `persisting compute [plan.py] …`
#[derive(Debug, Clone, Args)]
#[command(about = "Run a compute plan (`plan()` + `execute(item)`).")]
pub struct ComputeArgs {
    /// Plan script (`plan()` / `execute`). Required unless `--self-test`.
    #[arg(value_name = "SCRIPT")]
    pub script: Option<PathBuf>,

    /// Built-in smoke test (no user plan).
    #[arg(long)]
    pub self_test: bool,

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

    /// L2 infrastructure retries when a worker ask fails (not semantic retry).
    #[arg(long, default_value_t = 2)]
    pub retries: u32,

    /// Durable unique sink directory (`ready.ndjson` + `failures.ndjson` + `checkpoint.json`).
    #[arg(long, value_name = "DIR")]
    pub sink: Option<PathBuf>,

    /// Resume from `--sink`: skip task ids already in ready/failures.
    #[arg(long)]
    pub resume: bool,

    /// Also append terminal results to a Vortex trajectory (requires `--sink`).
    /// Writes `compute.result` / `compute.failure` events under traj storage.
    #[cfg(feature = "traj-sink")]
    #[arg(long)]
    pub traj: bool,

    /// Vortex storage root (default: `{sink}/traj`).
    #[cfg(feature = "traj-sink")]
    #[arg(long, value_name = "DIR")]
    pub traj_storage: Option<PathBuf>,

    /// Trajectory agent_id (default: `compute`).
    #[cfg(feature = "traj-sink")]
    #[arg(long, default_value = "compute")]
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

    /// More stderr tracing (`persisting_compute` + Pulsing at info).
    #[arg(short, long)]
    pub verbose: bool,

    /// Args forwarded to the plan script (`sys.argv[1:]`). Put after `--`.
    /// Example: `persisting compute task.py -- --model x --n 2`
    #[arg(last = true, value_name = "SCRIPT_ARGS")]
    pub script_args: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ResultsFormat {
    Ndjson,
    Summary,
    Quiet,
}

/// Run compute (async). Caller owns the Tokio runtime.
pub async fn run_compute(args: ComputeArgs) -> Result<ExitCode> {
    if args.self_test {
        let report = run_self_test(args.python, args.workers, args.verbose).await?;
        return Ok(if report.passed() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let Some(script) = args.script else {
        bail!("missing SCRIPT; pass a plan file, or use --self-test");
    };

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
            let vortex = crate::sink_traj::VortexResultSink::new(
                traj_storage.display().to_string(),
                args.traj_agent.clone(),
                session.clone(),
            );
            if let Ok(ledger) = CheckpointLedger::load(dir).await {
                vortex.seed_seen(ledger.skip_ids());
            }
            eprintln!(
                "[traj] vortex append → {}/{}/{}",
                traj_storage.display(),
                args.traj_agent,
                session
            );
            sinks.push(Box::new(vortex));
        }
        Some(Arc::new(TeeSink::new(sinks)))
    } else {
        None
    };

    if args.resume && file_sink.is_none() {
        bail!("--resume requires --sink DIR");
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
            let skip = ledger.skip_ids();
            eprintln!(
                "[ckpt] resume: ready={} fail={} skip_total={}",
                ledger.ready.len(),
                ledger.failed.len(),
                skip.len()
            );
            tracker.seed_from_ledger(&ledger);
            (SkipSet::from_iter(skip), Some(tracker))
        } else {
            (SkipSet::new(), Some(tracker))
        }
    } else {
        (SkipSet::new(), None)
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
        spawn_sink_writer(
            Arc::clone(sink),
            checkpoint.clone(),
            Some(skip_task_ids.clone()),
            max_inflight.saturating_mul(2).max(16),
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

    if matches!(results_fmt, ResultsFormat::Summary) {
        let ok = collected.iter().filter(|r| r.ok && !r.cancelled).count();
        println!(
            "{}",
            serde_json::json!({
                "total": collected.len(),
                "ok": ok,
                "failed": failed,
                "cancelled": collected.iter().filter(|r| r.cancelled).count(),
                "sink": args.sink.as_ref().map(|p| p.display().to_string()),
            })
        );
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

/// Ensure tracing is initialized once (safe to call from nested CLI).
///
/// Default is quiet: hush Pulsing actor lifecycle noise. Override with `RUST_LOG`,
/// or pass `--verbose` for `persisting_compute=info,pulsing_actor=info`.
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
                        "persisting_compute=info,pulsing_actor=info,info",
                    )
                } else {
                    // Quiet default: only warn+ from Pulsing; compute stays at warn.
                    tracing_subscriber::EnvFilter::new(
                        "persisting_compute=warn,pulsing_actor=warn,warn",
                    )
                }
            }),
        )
        .with_writer(std::io::stderr)
        .with_target(verbose)
        .try_init();
}
