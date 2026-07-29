//! pPilot task placement: least-loaded slots with optional sticky preference.
//!
//! **Primitive:** [`Scheduler`] · [`SlotGuard`] · [`WorkerPool`].
//!
//! When the runtime flattens `--per-worker` into one actor+host per slot,
//! construct with `Scheduler::new(n_slots, 1)`. Slot-major pool ordering
//! (see [`crate::dist::DistEnv::slot_names`]) keeps least-loaded spreading
//! across logical workers before filling a second slot on the same worker.
//!
//! Consecutive infra ask failures quarantine a slot for the rest of the job
//! so placement skips known-bad hosts.

use pulsing_actor::prelude::ActorRef;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;

pub type WorkerPool = Arc<RwLock<Vec<ActorRef>>>;

/// Consecutive infra failures before a slot is quarantined (job-local).
pub const DEFAULT_QUARANTINE_AFTER: usize = 3;

/// Shared placement state for one fleet run.
pub struct Scheduler {
    loads: Vec<AtomicUsize>,
    /// Max concurrent Executes per worker (typically 1 for long/uneven tasks).
    per_worker: usize,
    notify: Notify,
    consecutive_failures: Vec<AtomicUsize>,
    quarantined: Vec<AtomicBool>,
    quarantine_after: usize,
}

impl Scheduler {
    pub fn new(n_workers: usize, per_worker: usize) -> Arc<Self> {
        Self::with_quarantine_after(n_workers, per_worker, DEFAULT_QUARANTINE_AFTER)
    }

    pub fn with_quarantine_after(
        n_workers: usize,
        per_worker: usize,
        quarantine_after: usize,
    ) -> Arc<Self> {
        let per_worker = per_worker.max(1);
        let n_workers = n_workers.max(1);
        let quarantine_after = quarantine_after.max(1);
        Arc::new(Self {
            loads: (0..n_workers).map(|_| AtomicUsize::new(0)).collect(),
            per_worker,
            notify: Notify::new(),
            consecutive_failures: (0..n_workers).map(|_| AtomicUsize::new(0)).collect(),
            quarantined: (0..n_workers).map(|_| AtomicBool::new(false)).collect(),
            quarantine_after,
        })
    }

    pub fn worker_count(&self) -> usize {
        self.loads.len()
    }

    pub fn per_worker(&self) -> usize {
        self.per_worker
    }

    pub fn capacity(&self) -> usize {
        self.active_slots().saturating_mul(self.per_worker).max(1)
    }

    /// Slots not quarantined.
    pub fn active_slots(&self) -> usize {
        self.quarantined
            .iter()
            .filter(|q| !q.load(Ordering::Acquire))
            .count()
    }

    pub fn is_quarantined(&self, index: usize) -> bool {
        self.quarantined
            .get(index)
            .map(|q| q.load(Ordering::Acquire))
            .unwrap_or(true)
    }

    /// Record a successful ask — clears consecutive failure streak.
    pub fn note_success(&self, index: usize) {
        if index < self.consecutive_failures.len() {
            self.consecutive_failures[index].store(0, Ordering::Release);
        }
    }

    /// Record an infra ask failure; may quarantine the slot.
    pub fn note_failure(&self, index: usize) {
        if index >= self.consecutive_failures.len() {
            return;
        }
        let n = self.consecutive_failures[index].fetch_add(1, Ordering::AcqRel) + 1;
        if n >= self.quarantine_after && !self.quarantined[index].swap(true, Ordering::AcqRel) {
            tracing::warn!(
                slot = index,
                failures = n,
                "quarantining slot after consecutive infra failures"
            );
            self.notify.notify_waiters();
        }
    }

    /// Immediately quarantine a slot (DeathWatch / explicit drop).
    pub fn force_quarantine(&self, index: usize) {
        if index >= self.quarantined.len() {
            return;
        }
        if !self.quarantined[index].swap(true, Ordering::AcqRel) {
            tracing::warn!(slot = index, "quarantining slot (forced)");
            self.notify.notify_waiters();
        }
    }

    /// Reserve a worker slot (least in-flight among those under capacity).
    pub async fn acquire(&self) -> Result<usize, AcquireError> {
        loop {
            if let Some(i) = self.try_acquire() {
                return Ok(i);
            }
            if self.active_slots() == 0 {
                tracing::error!("all slots quarantined; placement fail-fast");
                return Err(AcquireError::AllQuarantined);
            }
            self.notify.notified().await;
        }
    }

    fn slot_usable(&self, i: usize, cur: usize) -> bool {
        !self.quarantined[i].load(Ordering::Acquire) && cur < self.per_worker
    }

    fn try_acquire(&self) -> Option<usize> {
        loop {
            let mut best: Option<(usize, usize)> = None;
            for (i, cell) in self.loads.iter().enumerate() {
                let cur = cell.load(Ordering::Acquire);
                if !self.slot_usable(i, cur) {
                    continue;
                }
                match best {
                    None => best = Some((i, cur)),
                    Some((_, b)) if cur < b => best = Some((i, cur)),
                    Some((bi, b)) if cur == b && i < bi => best = Some((i, cur)),
                    _ => {}
                }
            }
            let (i, expect) = best?;
            match self.loads[i].compare_exchange(
                expect,
                expect + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(i),
                Err(_) => continue,
            }
        }
    }

    /// Prefer `prefer` when that slot still has capacity; otherwise least-loaded.
    pub async fn acquire_prefer(&self, prefer: Option<usize>) -> Result<usize, AcquireError> {
        loop {
            if let Some(i) = prefer {
                if self.try_acquire_index(i) {
                    return Ok(i);
                }
            }
            if let Some(i) = self.try_acquire() {
                return Ok(i);
            }
            if self.active_slots() == 0 {
                tracing::error!("all slots quarantined; placement fail-fast");
                return Err(AcquireError::AllQuarantined);
            }
            self.notify.notified().await;
        }
    }

    /// Acquire **only** `slot` (sticky-after-contact). Err if quarantined.
    pub async fn acquire_sticky(&self, slot: usize) -> Result<usize, StickyLost> {
        loop {
            if self.is_quarantined(slot) {
                return Err(StickyLost::Quarantined(slot));
            }
            if self.try_acquire_index(slot) {
                return Ok(slot);
            }
            self.notify.notified().await;
        }
    }

    fn try_acquire_index(&self, index: usize) -> bool {
        if index >= self.loads.len() || self.quarantined[index].load(Ordering::Acquire) {
            return false;
        }
        loop {
            let cur = self.loads[index].load(Ordering::Acquire);
            if cur >= self.per_worker {
                return false;
            }
            match self.loads[index].compare_exchange(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    pub fn release(&self, index: usize) {
        if index >= self.loads.len() {
            return;
        }
        let prev = self.loads[index].fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "scheduler release underflow");
        self.notify.notify_waiters();
    }

    pub fn load_snapshot(&self) -> Vec<usize> {
        self.loads
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect()
    }

    pub fn quarantine_snapshot(&self) -> Vec<bool> {
        self.quarantined
            .iter()
            .map(|q| q.load(Ordering::Relaxed))
            .collect()
    }
}

/// RAII guard that releases a scheduler slot when dropped.
pub struct SlotGuard {
    sched: Arc<Scheduler>,
    index: usize,
}

impl SlotGuard {
    pub fn index(&self) -> usize {
        self.index
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.sched.release(self.index);
    }
}

impl Scheduler {
    pub async fn acquire_guard(self: &Arc<Self>) -> Result<SlotGuard, AcquireError> {
        let index = self.acquire().await?;
        Ok(SlotGuard {
            sched: Arc::clone(self),
            index,
        })
    }

    pub async fn acquire_guard_prefer(
        self: &Arc<Self>,
        prefer: Option<usize>,
    ) -> Result<SlotGuard, AcquireError> {
        let index = self.acquire_prefer(prefer).await?;
        Ok(SlotGuard {
            sched: Arc::clone(self),
            index,
        })
    }

    pub async fn acquire_guard_sticky(
        self: &Arc<Self>,
        slot: usize,
    ) -> Result<SlotGuard, StickyLost> {
        let index = self.acquire_sticky(slot).await?;
        Ok(SlotGuard {
            sched: Arc::clone(self),
            index,
        })
    }
}

/// No usable slots remain (all quarantined).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireError {
    AllQuarantined,
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcquireError::AllQuarantined => write!(f, "all worker slots quarantined"),
        }
    }
}

/// Sticky-after-contact placement cannot continue on this slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickyLost {
    Quarantined(usize),
}

impl std::fmt::Display for StickyLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StickyLost::Quarantined(s) => {
                write!(
                    f,
                    "sticky slot {s} quarantined (refuse cross-slot re-execute)"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prefers_idle_worker() {
        let s = Scheduler::new(3, 2);
        let a = s.acquire().await.unwrap();
        assert_eq!(a, 0);
        let b = s.acquire().await.unwrap();
        assert_eq!(b, 1);
        let c = s.acquire().await.unwrap();
        assert_eq!(c, 2);
        let d = s.acquire().await.unwrap();
        assert_eq!(d, 0);
        s.release(b);
        let e = s.acquire().await.unwrap();
        assert_eq!(e, 1);
        s.release(a);
        s.release(c);
        s.release(d);
        s.release(e);
        assert_eq!(s.load_snapshot(), vec![0, 0, 0]);
    }

    #[tokio::test]
    async fn acquire_prefer_sticky_when_free() {
        let s = Scheduler::new(3, 1);
        let a = s.acquire_prefer(None).await.unwrap();
        assert_eq!(a, 0);
        s.release(a);
        let b = s.acquire_prefer(Some(2)).await.unwrap();
        assert_eq!(b, 2);
        s.release(b);
    }

    #[tokio::test]
    async fn blocks_when_full_then_unblocks() {
        let s = Scheduler::new(1, 1);
        let g = s.acquire_guard().await.unwrap();
        let s2 = Arc::clone(&s);
        let handle = tokio::spawn(async move { s2.acquire().await });
        tokio::task::yield_now().await;
        assert!(!handle.is_finished());
        drop(g);
        let idx = handle.await.unwrap().unwrap();
        assert_eq!(idx, 0);
        s.release(idx);
    }

    #[tokio::test]
    async fn flat_pool_first_wave_spreads_across_slot0() {
        let s = Scheduler::new(6, 1);
        let mut got = Vec::new();
        for _ in 0..3 {
            got.push(s.acquire().await.unwrap());
        }
        assert_eq!(got, vec![0, 1, 2]);
        for i in got {
            s.release(i);
        }
    }

    #[tokio::test]
    async fn concurrent_acquire_fills_distinct_slots() {
        let s = Arc::clone(&Scheduler::new(4, 1));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let s2 = Arc::clone(&s);
            handles.push(tokio::spawn(async move { s2.acquire().await }));
        }
        let mut got: Vec<usize> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap().unwrap())
            .collect();
        got.sort();
        assert_eq!(got, vec![0, 1, 2, 3]);
        for i in got {
            s.release(i);
        }
    }

    #[tokio::test]
    async fn quarantine_skips_slot_on_acquire() {
        let s = Scheduler::with_quarantine_after(2, 1, 2);
        s.note_failure(0);
        s.note_failure(0);
        assert!(s.is_quarantined(0));
        let g = s.acquire_guard().await.unwrap();
        assert_eq!(g.index(), 1);
        drop(g);
        let g2 = s.acquire_guard_prefer(Some(0)).await.unwrap();
        assert_eq!(g2.index(), 1);
    }

    #[tokio::test]
    async fn acquire_sticky_errors_when_quarantined() {
        let s = Scheduler::new(2, 1);
        s.force_quarantine(0);
        assert!(matches!(
            s.acquire_sticky(0).await,
            Err(StickyLost::Quarantined(0))
        ));
        assert_eq!(s.acquire_sticky(1).await.unwrap(), 1);
        s.release(1);
    }

    #[tokio::test]
    async fn all_quarantined_acquire_fail_fast() {
        let s = Scheduler::with_quarantine_after(2, 1, 1);
        s.note_failure(0);
        s.note_failure(1);
        assert_eq!(s.active_slots(), 0);
        assert!(matches!(
            s.acquire().await,
            Err(AcquireError::AllQuarantined)
        ));
        assert!(matches!(
            s.acquire_prefer(Some(0)).await,
            Err(AcquireError::AllQuarantined)
        ));
    }

    #[tokio::test]
    async fn success_resets_failure_streak() {
        let s = Scheduler::with_quarantine_after(1, 1, 3);
        s.note_failure(0);
        s.note_failure(0);
        s.note_success(0);
        s.note_failure(0);
        s.note_failure(0);
        assert!(!s.is_quarantined(0));
    }
}
