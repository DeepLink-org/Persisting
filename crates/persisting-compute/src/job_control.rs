//! Side-channel job cancel + local DeathWatch for Pulsing fleets.
//!
//! WorkerActor mailboxes are serial: an in-flight `Execute` holds `receive`, so a
//! `Cancel` sitting behind it cannot stop Python. This actor shares the process
//! [`CancellationToken`] on a **separate** mailbox.
//!
//! Pulsing `watch` only supports **local** targets; we register local slot
//! ActorIds here and quarantine them in [`Scheduler`] on termination.

use crate::dist::DistEnv;
use crate::pulsing_ext::{ask_timeout, resolve_actor, ASK_TIMEOUT};
use crate::scheduler::Scheduler;
use futures::future::join_all;
use pulsing_actor::actor::{ActorId, StopReason};
use pulsing_actor::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobControlCommand {
    /// Cancel the shared job token (in-flight execute hosts select! + kill).
    Cancel,
    /// Register a local slot for DeathWatch → quarantine on stop.
    /// `slot` is the **flat pool index** (slot-major), not a per-rank ordinal.
    WatchSlot { slot: usize, actor_id: ActorId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JobControlReply {
    Ack { already: bool },
    Watched,
}

pub struct JobControlActor {
    job_cancel: CancellationToken,
    sched: Option<Arc<Scheduler>>,
    /// actor_id → slot index (local watches only).
    watched: Mutex<HashMap<ActorId, usize>>,
}

impl JobControlActor {
    pub fn new(job_cancel: CancellationToken) -> Self {
        Self {
            job_cancel,
            sched: None,
            watched: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_scheduler(job_cancel: CancellationToken, sched: Arc<Scheduler>) -> Self {
        Self {
            job_cancel,
            sched: Some(sched),
            watched: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl Actor for JobControlActor {
    fn metadata(&self) -> HashMap<String, String> {
        HashMap::from([
            ("role".into(), "compute-job-control".into()),
            (
                "cancelled".into(),
                self.job_cancel.is_cancelled().to_string(),
            ),
        ])
    }

    async fn receive(
        &mut self,
        msg: Message,
        ctx: &mut ActorContext,
    ) -> pulsing_actor::error::Result<Message> {
        // DeathWatch notification: (ActorId, StopReason) — parse borrows so we
        // can still unpack JobControlCommand on the ask path.
        if let Ok((dead_id, reason)) = msg.parse::<(ActorId, StopReason)>() {
            let slot = self
                .watched
                .lock()
                .ok()
                .and_then(|g| g.get(&dead_id).copied());
            if let Some(slot) = slot {
                tracing::warn!(
                    %dead_id,
                    slot,
                    %reason,
                    "DeathWatch: local worker terminated → quarantine"
                );
                if let Some(sched) = &self.sched {
                    sched.force_quarantine(slot);
                }
            }
            return Message::pack(&JobControlReply::Ack { already: true });
        }

        let cmd: JobControlCommand = msg.unpack()?;
        let reply = match cmd {
            JobControlCommand::Cancel => {
                let already = self.job_cancel.is_cancelled();
                if !already {
                    tracing::warn!("job control: cancelling shared job token");
                    self.job_cancel.cancel();
                }
                JobControlReply::Ack { already }
            }
            JobControlCommand::WatchSlot { slot, actor_id } => {
                if let Ok(mut g) = self.watched.lock() {
                    g.insert(actor_id, slot);
                }
                if let Err(e) = ctx.watch(&actor_id).await {
                    tracing::warn!(%actor_id, slot, error = %e, "DeathWatch register failed");
                } else {
                    tracing::debug!(%actor_id, slot, "DeathWatch registered");
                }
                JobControlReply::Watched
            }
        };
        Message::pack(&reply)
    }
}

/// Ask every rank's job-control actor to cancel (best-effort, parallel).
pub async fn broadcast_job_cancel(system: &Arc<ActorSystem>, world_size: usize) {
    let world_size = world_size.max(1);
    let mut futs = Vec::with_capacity(world_size);
    for rank in 0..world_size {
        let name = DistEnv::job_control_name(rank);
        let system = Arc::clone(system);
        futs.push(async move {
            match resolve_actor(system.as_ref(), &name).await {
                Ok(ctrl) => match ask_timeout::<_, JobControlReply>(
                    &ctrl,
                    JobControlCommand::Cancel,
                    ASK_TIMEOUT,
                )
                .await
                {
                    Ok(JobControlReply::Ack { already }) => {
                        tracing::debug!(%name, already, "job cancel broadcast ok");
                    }
                    Ok(_) => tracing::debug!(%name, "job cancel unexpected reply"),
                    Err(e) => tracing::warn!(%name, error = %e, "job cancel ask failed"),
                },
                Err(e) => tracing::warn!(%name, error = %e, "job cancel resolve failed"),
            }
        });
    }
    join_all(futs).await;
}

/// Register DeathWatch for local slots.
///
/// Each entry is `(actor_ref, flat_pool_index)`. Callers must pass the same
/// slot-major indices used by [`crate::scheduler::Scheduler`] / [`DistEnv::slot_names`]
/// — **not** `0..local_count` when the fleet spans multiple ranks.
pub async fn register_local_watches(
    control: &ActorRef,
    slots: &[(ActorRef, usize)],
) -> anyhow::Result<()> {
    for (wref, flat_idx) in slots {
        let _ = ask_timeout::<_, JobControlReply>(
            control,
            JobControlCommand::WatchSlot {
                slot: *flat_idx,
                actor_id: *wref.id(),
            },
            Duration::from_secs(5),
        )
        .await?;
    }
    Ok(())
}

/// Watch local `job_cancel`; when it fires, fan-out to all ranks' control actors.
pub fn spawn_cancel_broadcast(
    system: Arc<ActorSystem>,
    job_cancel: CancellationToken,
    world_size: usize,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        job_cancel.cancelled().await;
        tracing::warn!(
            world_size,
            "local job cancel: broadcasting to job-control actors"
        );
        broadcast_job_cancel(&system, world_size).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_command_trips_shared_token() {
        let token = CancellationToken::new();
        let system = Arc::new(
            ActorSystem::builder()
                .mailbox_capacity(16)
                .build()
                .await
                .unwrap(),
        );
        let name = DistEnv::job_control_name(0);
        let ctrl = system
            .spawn_named(&name, JobControlActor::new(token.clone()))
            .await
            .unwrap();
        assert!(!token.is_cancelled());
        let ack = ctrl
            .ask::<_, JobControlReply>(JobControlCommand::Cancel)
            .await
            .unwrap();
        assert!(matches!(ack, JobControlReply::Ack { already: false }));
        assert!(token.is_cancelled());
        system.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broadcast_reaches_peer_rank_control() {
        let t0 = CancellationToken::new();
        let t1 = CancellationToken::new();
        let system = Arc::new(
            ActorSystem::builder()
                .mailbox_capacity(16)
                .build()
                .await
                .unwrap(),
        );
        system
            .spawn_named(
                &DistEnv::job_control_name(0),
                JobControlActor::new(t0.clone()),
            )
            .await
            .unwrap();
        system
            .spawn_named(
                &DistEnv::job_control_name(1),
                JobControlActor::new(t1.clone()),
            )
            .await
            .unwrap();
        broadcast_job_cancel(&system, 2).await;
        assert!(t0.is_cancelled() && t1.is_cancelled());
        system.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn death_watch_quarantines_local_slot() {
        let token = CancellationToken::new();
        let sched = Scheduler::new(1, 1);
        let system = Arc::new(
            ActorSystem::builder()
                .mailbox_capacity(16)
                .build()
                .await
                .unwrap(),
        );
        let ctrl = system
            .spawn_named(
                &DistEnv::job_control_name(0),
                JobControlActor::with_scheduler(token.clone(), Arc::clone(&sched)),
            )
            .await
            .unwrap();
        let worker = system
            .spawn_named(
                "compute/worker/0",
                crate::worker::WorkerActor::with_plan(
                    "w0",
                    std::path::PathBuf::from("python3"),
                    vec![],
                    std::path::PathBuf::from("/dev/null"),
                    vec![],
                    token.clone(),
                ),
            )
            .await
            .unwrap();
        register_local_watches(&ctrl, &[(worker.clone(), 0)])
            .await
            .unwrap();
        assert!(!sched.is_quarantined(0));
        system.stop("compute/worker/0").await.unwrap();
        // Allow DeathWatch delivery.
        for _ in 0..50 {
            if sched.is_quarantined(0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(sched.is_quarantined(0), "expected DeathWatch quarantine");
        system.shutdown().await.unwrap();
    }

    /// Regression: torchrun-shaped pool (world=2, per_worker=2) must map
    /// rank0 slot1 → flat index 2, not local ordinal 1 (which is peer w1s0).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn death_watch_uses_slot_major_flat_index() {
        let token = CancellationToken::new();
        let world = 2;
        let per_worker = 2;
        let n_slots = world * per_worker;
        let sched = Scheduler::new(n_slots, 1);
        let system = Arc::new(
            ActorSystem::builder()
                .mailbox_capacity(16)
                .build()
                .await
                .unwrap(),
        );
        let ctrl = system
            .spawn_named(
                &DistEnv::job_control_name(0),
                JobControlActor::with_scheduler(token.clone(), Arc::clone(&sched)),
            )
            .await
            .unwrap();

        // Only spawn rank0's two local slots (as driver does before peers join).
        let mut watches = Vec::new();
        for slot in 0..per_worker {
            let name = DistEnv::slot_name(0, slot, per_worker);
            let w = system
                .spawn_named(
                    &name,
                    crate::worker::WorkerActor::with_plan(
                        format!("w0s{slot}"),
                        std::path::PathBuf::from("python3"),
                        vec![],
                        std::path::PathBuf::from("/dev/null"),
                        vec![],
                        token.clone(),
                    ),
                )
                .await
                .unwrap();
            let flat = DistEnv::slot_flat_index(0, slot, world, per_worker);
            watches.push((w, flat));
        }
        assert_eq!(watches[0].1, 0);
        assert_eq!(watches[1].1, 2, "w0s1 must be flat 2, not 1");

        register_local_watches(&ctrl, &watches).await.unwrap();

        // Kill rank0 slot1 → must quarantine flat 2, leave 1 (peer) alone.
        system
            .stop(&DistEnv::slot_name(0, 1, per_worker))
            .await
            .unwrap();
        for _ in 0..50 {
            if sched.is_quarantined(2) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            sched.is_quarantined(2),
            "expected quarantine of flat index 2 (w0s1)"
        );
        assert!(
            !sched.is_quarantined(1),
            "must not quarantine flat 1 (would be peer w1s0)"
        );
        assert!(!sched.is_quarantined(0));
        system.shutdown().await.unwrap();
    }
}
