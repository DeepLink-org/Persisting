//! Boot Pulsing fleet, then hand the job to [`crate::driver::Driver`].
//!
//! - Local `-w N`: spawn N×`--per-worker` slot actors (each owns a Python host).
//! - torchrun: each rank spawns `--per-worker` slot actors; rank0 is Driver.
//!
//! `--per-worker N` = N concurrent Execute slots per logical worker/rank. Each
//! slot is its own [`WorkerActor`] (serial mailbox) + dedicated plan host, so
//! concurrency is real — not mailbox queuing on one actor.
//!
//! Workers are spawned via [`crate::pulsing_ext::spawn_supervised`] (factory +
//! `SupervisionSpec`) so Pulsing can restart a failed slot.

use crate::dist::DistEnv;
use crate::driver::Driver;
use crate::job_control::{register_local_watches, spawn_cancel_broadcast, JobControlActor};
use crate::observe::spawn_snapshot_loop;
use crate::pulsing_ext::{ask_timeout, resolve_actor, spawn_supervised, ASK_TIMEOUT};
use crate::python_env::pythonpath_for_script;
use crate::scheduler::{Scheduler, WorkerPool};
use crate::supervisor::{EmbeddedSupervisor, EmbeddedSupervisorConfig};
use crate::task::TaskResult;
use crate::worker::{ShutdownGate, WorkerCommand, WorkerConfig, WorkerReply};
use anyhow::{bail, Context, Result};
use pulsing_actor::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

pub use crate::driver::RunOptions;

/// Entry: torchrun env → distributed; else in-process local fleet.
pub async fn run_fleet(
    opts: RunOptions,
    on_result: impl FnMut(TaskResult) + Send,
) -> Result<Vec<TaskResult>> {
    if let Some(dist) = DistEnv::from_env()? {
        tracing::debug!(
            rank = dist.rank,
            world_size = dist.world_size,
            seed = %dist.pulsing_seed,
            "torchrun placement detected"
        );
        if dist.is_driver() {
            run_driver_rank(dist, opts, on_result).await
        } else {
            let pp = apply_pythonpath(&opts);
            run_worker_rank(dist, &opts, pp).await?;
            Ok(Vec::new())
        }
    } else {
        run_local_fleet(opts, on_result).await
    }
}

/// Single-process: spawn N×P slot workers; this process runs the Driver.
pub async fn run_local_fleet(
    opts: RunOptions,
    on_result: impl FnMut(TaskResult) + Send,
) -> Result<Vec<TaskResult>> {
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig {
        network_limit_bytes_per_second: None,
        ..EmbeddedSupervisorConfig::default()
    })
    .await
    .context("start embedded pPilot Supervisor")?;
    let mut supervisor_bootstrap = supervisor.bootstrap();
    if let Some(coordinator) = &opts.coordinator {
        supervisor_bootstrap.attempt_registry_uri =
            Some(coordinator.control().root_uri().to_string());
        supervisor_bootstrap.attempt_ttl_ms = opts
            .coordinator
            .as_ref()
            .map_or(15_000, |coordinator| coordinator.lease_ttl_ms());
    }
    let pythonpath = apply_pythonpath(&opts);
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(256)
        .build()
        .await
        .context("build ActorSystem")?;
    let per_worker = opts.per_worker_inflight.max(1);
    let n_workers = opts.workers.max(1);
    let names = DistEnv::slot_names(n_workers, per_worker);
    let n_slots = names.len();
    // Second arg is always 1: pool is already flattened one-actor-per-slot.
    let sched = Scheduler::new(n_slots, 1);

    let control = spawn_job_control(
        &system,
        0,
        opts.job_cancel.clone(),
        Some(Arc::clone(&sched)),
    )
    .await?;
    let cancel_fanout = spawn_cancel_broadcast(Arc::clone(&system), opts.job_cancel.clone(), 1);

    let watches = spawn_local_fleet_slots(
        &system,
        n_workers,
        per_worker,
        &opts,
        &pythonpath,
        Some(supervisor_bootstrap),
    )
    .await?;
    let pool: WorkerPool = Arc::new(RwLock::new(
        watches.iter().map(|(r, _)| r.clone()).collect(),
    ));
    register_local_watches(&control, &watches).await?;

    tracing::debug!(
        workers = n_workers,
        per_worker,
        slots = n_slots,
        capacity = sched.capacity(),
        "driver ready (local fleet)"
    );

    let result = run_driver_loop(pool, sched, &opts, on_result, cancel_fanout, system, None).await;
    if let Err(error) = supervisor.shutdown().await {
        tracing::warn!(%error, "failed to stop embedded pPilot Supervisor");
    }
    result
}

/// Rank0 under torchrun: bind Pulsing seed, wait for peer slots, run Driver.
async fn run_driver_rank(
    dist: DistEnv,
    opts: RunOptions,
    on_result: impl FnMut(TaskResult) + Send,
) -> Result<Vec<TaskResult>> {
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig {
        network_limit_bytes_per_second: None,
        ..EmbeddedSupervisorConfig::default()
    })
    .await
    .context("start rank-local pPilot Supervisor")?;
    let mut supervisor_bootstrap = supervisor.bootstrap();
    if let Some(coordinator) = &opts.coordinator {
        supervisor_bootstrap.attempt_registry_uri =
            Some(coordinator.control().root_uri().to_string());
        supervisor_bootstrap.attempt_ttl_ms = coordinator.lease_ttl_ms();
    }
    let bind = format!("0.0.0.0:{}", dist.pulsing_seed.port());
    let system: Arc<ActorSystem> = ActorSystem::builder()
        .mailbox_capacity(256)
        .addr(bind.as_str())
        .build()
        .await
        .context("build driver ActorSystem")?;
    tracing::debug!(
        advertised = %dist.pulsing_seed,
        bound = %system.addr(),
        "driver Pulsing listening (peers join via MASTER_ADDR seed)"
    );

    let per_worker = opts.per_worker_inflight.max(1);
    let names = DistEnv::slot_names(dist.world_size, per_worker);
    let n_slots = names.len();
    // Second arg is always 1: pool is already flattened one-actor-per-slot.
    let sched = Scheduler::new(n_slots, 1);

    let control = spawn_job_control(
        &system,
        0,
        opts.job_cancel.clone(),
        Some(Arc::clone(&sched)),
    )
    .await?;

    let pythonpath = apply_pythonpath(&opts);
    let local_watches = spawn_rank_slots(
        &system,
        RankPlacement {
            rank: 0,
            n_workers: dist.world_size,
            per_worker,
        },
        &opts,
        &pythonpath,
        None,
        Some(supervisor_bootstrap),
    )
    .await?;
    let pool: WorkerPool = Arc::new(RwLock::new(
        local_watches.iter().map(|(r, _)| r.clone()).collect(),
    ));
    register_local_watches(&control, &local_watches).await?;

    wait_and_fill_workers(&system, &pool, &names, Duration::from_secs(120)).await?;

    let cancel_fanout = spawn_cancel_broadcast(
        Arc::clone(&system),
        opts.job_cancel.clone(),
        dist.world_size,
    );

    tracing::debug!(
        world_size = dist.world_size,
        per_worker,
        slots = n_slots,
        capacity = sched.capacity(),
        "driver ready (torchrun)"
    );

    let world_size = dist.world_size;
    let per_worker_shutdown = per_worker;
    let result = run_driver_loop(
        pool,
        sched,
        &opts,
        on_result,
        cancel_fanout,
        system,
        Some(DriverPostShutdown {
            names,
            world_size,
            per_worker: per_worker_shutdown,
        }),
    )
    .await;
    if let Err(error) = supervisor.shutdown().await {
        tracing::warn!(%error, "failed to stop rank-local pPilot Supervisor");
    }
    result
}

/// Rank > 0: join Pulsing, serve `--per-worker` slot actors until Shutdown.
async fn run_worker_rank(dist: DistEnv, opts: &RunOptions, pythonpath: Vec<PathBuf>) -> Result<()> {
    let supervisor = EmbeddedSupervisor::start(EmbeddedSupervisorConfig {
        network_limit_bytes_per_second: None,
        ..EmbeddedSupervisorConfig::default()
    })
    .await
    .context("start worker-local pPilot Supervisor")?;
    let mut supervisor_bootstrap = supervisor.bootstrap();
    if let Some(coordinator) = &opts.coordinator {
        supervisor_bootstrap.attempt_registry_uri =
            Some(coordinator.control().root_uri().to_string());
        supervisor_bootstrap.attempt_ttl_ms = coordinator.lease_ttl_ms();
    }
    let seed = dist.pulsing_seed.to_string();
    let mut last = None;
    let system = {
        let mut built = None;
        for attempt in 1..=60 {
            match ActorSystem::builder()
                .mailbox_capacity(256)
                .addr("0.0.0.0:0")
                .seeds([seed.as_str()])
                .build()
                .await
            {
                Ok(s) => {
                    tracing::debug!(attempt, %seed, "joined Pulsing cluster");
                    built = Some(s);
                    break;
                }
                Err(e) => {
                    last = Some(e.to_string());
                    tracing::debug!(attempt, error = %e, "waiting for driver Pulsing seed");
                    sleep(Duration::from_millis(250)).await;
                }
            }
        }
        match built {
            Some(s) => s,
            None => bail!("failed to join Pulsing at {seed}: {last:?}"),
        }
    };

    if let Some(pp) = crate::python_env::merge_pythonpath(&pythonpath) {
        std::env::set_var("PYTHONPATH", pp);
    }

    spawn_job_control(&system, dist.rank, opts.job_cancel.clone(), None).await?;

    let per_worker = opts.per_worker_inflight.max(1);
    let gate = ShutdownGate::new(per_worker);
    let _slots = spawn_rank_slots(
        &system,
        RankPlacement {
            rank: dist.rank,
            n_workers: dist.world_size,
            per_worker,
        },
        opts,
        &pythonpath,
        Some(Arc::clone(&gate)),
        Some(supervisor_bootstrap),
    )
    .await?;

    gate.wait().await;
    tracing::debug!(rank = dist.rank, "all worker slots shutdown");
    system
        .shutdown()
        .await
        .map_err(|e| anyhow::anyhow!("shutdown: {e}"))?;
    supervisor.shutdown().await?;
    Ok(())
}

// ── shared boot helpers ───────────────────────────────────────────────

/// Resolve PYTHONPATH extras and optionally set the process env.
fn apply_pythonpath(opts: &RunOptions) -> Vec<PathBuf> {
    let mut extras = pythonpath_for_script(&opts.script);
    extras.extend(opts.pythonpath_extra.iter().cloned());
    if let Some(pp) = crate::python_env::merge_pythonpath(&extras) {
        std::env::set_var("PYTHONPATH", pp);
    }
    extras
}

fn worker_slot_id(worker: usize, slot: usize, per_worker: usize) -> String {
    if per_worker <= 1 {
        format!("w{worker}")
    } else {
        format!("w{worker}s{slot}")
    }
}

#[derive(Clone, Copy)]
struct SlotPlacement {
    worker: usize,
    slot: usize,
    n_workers: usize,
    per_worker: usize,
}

async fn spawn_job_control(
    system: &Arc<ActorSystem>,
    rank: usize,
    job_cancel: CancellationToken,
    sched: Option<Arc<Scheduler>>,
) -> Result<ActorRef> {
    let name = DistEnv::job_control_name(rank);
    let actor = match sched {
        Some(s) => JobControlActor::with_scheduler(job_cancel, s),
        None => JobControlActor::new(job_cancel),
    };
    system
        .spawn_named(&name, actor)
        .await
        .map_err(|e| anyhow::anyhow!("spawn job control {name}: {e}"))
}

async fn spawn_one_slot(
    system: &Arc<ActorSystem>,
    placement: SlotPlacement,
    opts: &RunOptions,
    pythonpath: &[PathBuf],
    gate: Option<Arc<ShutdownGate>>,
    supervisor: Option<persisting_control::SupervisorBootstrap>,
) -> Result<(ActorRef, usize)> {
    let SlotPlacement {
        worker,
        slot,
        n_workers,
        per_worker,
    } = placement;
    let name = DistEnv::slot_name(worker, slot, per_worker);
    let cfg = WorkerConfig::with_fresh_cache(
        worker_slot_id(worker, slot, per_worker),
        opts.python.clone(),
        pythonpath.to_vec(),
        opts.script.clone(),
        opts.script_args.clone(),
        opts.job_cancel.clone(),
        gate,
    )
    .with_supervisor(supervisor);
    let wref = spawn_supervised(system, &name, move || Ok(cfg.build())).await?;
    let flat = DistEnv::slot_flat_index(worker, slot, n_workers, per_worker);
    tracing::debug!(%name, worker, slot, flat, "worker slot ready");
    Ok((wref, flat))
}

/// Spawn all slots for one rank (torchrun driver local / worker rank).
/// Returns `(ActorRef, slot-major flat index)` pairs.
#[derive(Clone, Copy)]
struct RankPlacement {
    rank: usize,
    n_workers: usize,
    per_worker: usize,
}

async fn spawn_rank_slots(
    system: &Arc<ActorSystem>,
    placement: RankPlacement,
    opts: &RunOptions,
    pythonpath: &[PathBuf],
    gate: Option<Arc<ShutdownGate>>,
    supervisor: Option<persisting_control::SupervisorBootstrap>,
) -> Result<Vec<(ActorRef, usize)>> {
    let RankPlacement {
        rank,
        n_workers,
        per_worker,
    } = placement;
    let mut out = Vec::with_capacity(per_worker);
    for slot in 0..per_worker {
        out.push(
            spawn_one_slot(
                system,
                SlotPlacement {
                    worker: rank,
                    slot,
                    n_workers,
                    per_worker,
                },
                opts,
                pythonpath,
                gate.clone(),
                supervisor.clone(),
            )
            .await?,
        );
    }
    Ok(out)
}

/// Local fleet: slot-major order over all workers (matches [`DistEnv::slot_names`]).
async fn spawn_local_fleet_slots(
    system: &Arc<ActorSystem>,
    n_workers: usize,
    per_worker: usize,
    opts: &RunOptions,
    pythonpath: &[PathBuf],
    supervisor: Option<persisting_control::SupervisorBootstrap>,
) -> Result<Vec<(ActorRef, usize)>> {
    let mut out = Vec::with_capacity(n_workers.saturating_mul(per_worker));
    for slot in 0..per_worker {
        for worker in 0..n_workers {
            out.push(
                spawn_one_slot(
                    system,
                    SlotPlacement {
                        worker,
                        slot,
                        n_workers,
                        per_worker,
                    },
                    opts,
                    pythonpath,
                    None,
                    supervisor.clone(),
                )
                .await?,
            );
        }
    }
    Ok(out)
}

struct DriverPostShutdown {
    names: Vec<String>,
    world_size: usize,
    per_worker: usize,
}

/// Shared Driver + observe snapshot + system shutdown.
async fn run_driver_loop(
    pool: WorkerPool,
    sched: Arc<Scheduler>,
    opts: &RunOptions,
    on_result: impl FnMut(TaskResult) + Send,
    cancel_fanout: tokio::task::JoinHandle<()>,
    system: Arc<ActorSystem>,
    post: Option<DriverPostShutdown>,
) -> Result<Vec<TaskResult>> {
    let driver = Driver::new(Arc::clone(&pool), Arc::clone(&sched));
    let snap = spawn_snapshot_loop(
        Arc::clone(&opts.observer),
        Arc::clone(&sched),
        opts.job_cancel.clone(),
        Duration::from_secs(1),
    );
    let out = driver.run_plan(opts, on_result).await?;
    snap.abort();
    cancel_fanout.abort();
    if let Some(p) = post {
        if opts.job_cancel.is_cancelled() {
            crate::job_control::broadcast_job_cancel(&system, p.world_size).await;
        }
        shutdown_workers_resolved(&system, &p.names, p.per_worker).await?;
    }
    system
        .shutdown()
        .await
        .map_err(|e| anyhow::anyhow!("shutdown: {e}"))?;
    Ok(out)
}

async fn wait_and_fill_workers(
    system: &Arc<ActorSystem>,
    pool: &WorkerPool,
    names: &[String],
    timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut refs = Vec::with_capacity(names.len());
        let mut missing = Vec::new();
        for name in names {
            match resolve_actor(system.as_ref(), name).await {
                Ok(r) => refs.push(r),
                Err(_) => missing.push(name.clone()),
            }
        }
        if missing.is_empty() {
            let mut g = pool
                .write()
                .map_err(|_| anyhow::anyhow!("worker pool lock"))?;
            *g = refs;
            tracing::debug!(n = g.len(), "all worker slots resolved");
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for workers: missing {missing:?}");
        }
        tracing::debug!(?missing, "waiting for worker gossip/resolve");
        sleep(Duration::from_millis(300)).await;
    }
}

async fn shutdown_workers_resolved(
    system: &Arc<ActorSystem>,
    names: &[String],
    per_worker: usize,
) -> Result<()> {
    for name in names {
        let is_local = (0..per_worker.max(1))
            .any(|slot| name == &DistEnv::slot_name(0, slot, per_worker.max(1)));
        if is_local {
            continue;
        }
        match resolve_actor(system.as_ref(), name).await {
            Ok(w) => {
                let _ =
                    ask_timeout::<_, WorkerReply>(&w, WorkerCommand::Shutdown, ASK_TIMEOUT).await;
            }
            Err(e) => tracing::warn!(%name, error = %e, "shutdown: resolve failed"),
        }
    }
    sleep(Duration::from_millis(200)).await;
    Ok(())
}
