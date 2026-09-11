//! Process-wide admission + AIMD backoff for remote object-store I/O.
//!
//! Lance opens and table writes against flaky S3-compatible gateways amplify
//! timeouts when several datasets race (list `_versions/`, retries, AIMD inside
//! object_store). This gate:
//! 1. caps concurrent remote Lance ops (default 1);
//! 2. after a transient failure, forces a shared cooldown + growing delay;
//! 3. decays the delay after a streak of successes.
//!
//! Local `file://` paths bypass the gate entirely.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const DEFAULT_REMOTE_CONCURRENCY: usize = 1;
const MAX_REMOTE_CONCURRENCY: usize = 2;
const MAX_DELAY_MS: u64 = 30_000;
const SUCCESS_STREAK_TO_DECAY: u32 = 4;

/// Whether the gated op is primarily reading metadata/objects or writing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoKind {
    Read,
    Write,
}

impl IoKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// Live UI event while the process-wide object-store gate is throttling.
#[derive(Debug, Clone)]
pub enum ObjectStoreThrottleEvent {
    Enter {
        kind: IoKind,
        /// Why the wait happened: `throttle` (AIMD sleep) or `admit` (semaphore).
        reason: &'static str,
        wait_ms: u64,
        delay_ms: u64,
        failures: u64,
    },
    /// Cooldown tick / backoff / recovery — UI should refresh AIMD fields.
    Update {
        kind: IoKind,
        /// `throttle` | `admit` | `backoff` | `recover` | `ok`
        reason: &'static str,
        wait_ms: u64,
        delay_ms: u64,
        failures: u64,
    },
    Leave {
        kind: IoKind,
    },
}

/// Point-in-time gate status for progress painting between waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectStoreGateSnapshot {
    pub kind: IoKind,
    /// Current AIMD delay applied before the next remote acquire (0 = healthy).
    pub delay_ms: u64,
    pub cooldown_remaining_ms: u64,
    pub failures: u64,
    /// Successes toward the next multiplicative decay (`/` [`SUCCESS_STREAK_TO_DECAY`]).
    pub success_streak: u32,
    pub success_streak_target: u32,
    pub active_waiters: u32,
    /// Semaphore slots still free / configured remote concurrency.
    pub available_permits: usize,
    pub max_permits: usize,
}

type ThrottleHook = Arc<dyn Fn(ObjectStoreThrottleEvent) + Send + Sync>;

fn throttle_hook_slot() -> &'static Mutex<Option<ThrottleHook>> {
    static SLOT: OnceLock<Mutex<Option<ThrottleHook>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Restores the previous throttle UI hook when dropped.
pub struct ObjectStoreThrottleHookGuard {
    previous: Option<ThrottleHook>,
}

impl Drop for ObjectStoreThrottleHookGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = throttle_hook_slot().lock() {
            *slot = self.previous.take();
        }
    }
}

/// Install a process-wide S3/object-store throttle listener for the current scope.
pub fn install_throttle_hook(
    hook: Arc<dyn Fn(ObjectStoreThrottleEvent) + Send + Sync>,
) -> ObjectStoreThrottleHookGuard {
    let previous = match throttle_hook_slot().lock() {
        Ok(mut slot) => slot.replace(hook),
        Err(_) => None,
    };
    ObjectStoreThrottleHookGuard { previous }
}

fn emit_throttle(event: ObjectStoreThrottleEvent) {
    let Ok(slot) = throttle_hook_slot().lock() else {
        return;
    };
    if let Some(hook) = slot.as_ref() {
        hook(event);
    }
}

#[derive(Debug)]
struct AimdState {
    /// Extra sleep applied before each remote acquire while degraded.
    delay_ms: u64,
    /// No new remote op starts until this instant.
    cooldown_until: Option<Instant>,
    successes_since_backoff: u32,
    failures: u64,
    /// Last classified op that hit the gate (for progress UI).
    last_kind: IoKind,
    /// Nested enter/leave count for active throttle waits.
    active_waiters: u32,
}

impl Default for AimdState {
    fn default() -> Self {
        Self {
            delay_ms: 0,
            cooldown_until: None,
            successes_since_backoff: 0,
            failures: 0,
            last_kind: IoKind::Read,
            active_waiters: 0,
        }
    }
}

struct Gate {
    semaphore: Arc<Semaphore>,
    concurrency: usize,
    state: Mutex<AimdState>,
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| {
        let concurrency = std::env::var("PCHRONICLE_OBJECT_STORE_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_REMOTE_CONCURRENCY)
            .clamp(1, MAX_REMOTE_CONCURRENCY);
        Gate {
            semaphore: Arc::new(Semaphore::new(concurrency)),
            concurrency,
            state: Mutex::new(AimdState::default()),
        }
    })
}

/// True for s3/gs/az (and similar) URIs; false for local paths / file://.
pub(crate) fn is_remote_uri(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once("://") else {
        return false;
    };
    !matches!(scheme, "file" | "file+uring" | "memory" | "shared-memory")
}

/// Snapshot AIMD / cooldown state for progress UI.
pub fn snapshot() -> ObjectStoreGateSnapshot {
    let g = gate();
    let available_permits = g.semaphore.available_permits();
    let max_permits = g.concurrency;
    let Ok(state) = g.state.lock() else {
        return ObjectStoreGateSnapshot {
            kind: IoKind::Read,
            delay_ms: 0,
            cooldown_remaining_ms: 0,
            failures: 0,
            success_streak: 0,
            success_streak_target: SUCCESS_STREAK_TO_DECAY,
            active_waiters: 0,
            available_permits,
            max_permits,
        };
    };
    let cooldown_remaining_ms = state
        .cooldown_until
        .and_then(|until| until.checked_duration_since(Instant::now()))
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    ObjectStoreGateSnapshot {
        kind: state.last_kind,
        delay_ms: state.delay_ms,
        cooldown_remaining_ms,
        failures: state.failures,
        success_streak: state.successes_since_backoff,
        success_streak_target: SUCCESS_STREAK_TO_DECAY,
        active_waiters: state.active_waiters,
        available_permits,
        max_permits,
    }
}

/// Compact AIMD label for progress brackets. All AIMD fields follow `aimd`.
pub fn format_aimd_flow_label(snap: &ObjectStoreGateSnapshot, event: Option<&str>) -> String {
    let permits = format!("p={}/{}", snap.available_permits, snap.max_permits);
    let streak = format!("s={}/{}", snap.success_streak, snap.success_streak_target);
    let event = event
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    let head = if event.is_empty() {
        "aimd".to_owned()
    } else {
        format!("aimd {event}")
    };
    if snap.cooldown_remaining_ms > 0 {
        return format!(
            "{head} cd={:.1}s d={}ms f={} {streak} w={} {permits}",
            snap.cooldown_remaining_ms as f32 / 1000.0,
            snap.delay_ms,
            snap.failures,
            snap.active_waiters,
        );
    }
    if snap.delay_ms > 0 || snap.failures > 0 || snap.active_waiters > 0 || !event.is_empty() {
        return format!(
            "{head} d={}ms f={} {streak} w={} {permits}",
            snap.delay_ms, snap.failures, snap.active_waiters,
        );
    }
    format!("{head} ok {streak} {permits}")
}

pub(crate) struct Permit {
    _permit: Option<OwnedSemaphorePermit>,
}

/// Acquire admission for a Lance/object-store operation on `uri`.
pub(crate) async fn acquire(uri: &str, kind: IoKind) -> Permit {
    if !is_remote_uri(uri) {
        return Permit { _permit: None };
    }
    if let Ok(mut state) = gate().state.lock() {
        state.last_kind = kind;
    }
    wait_out_degradation(kind).await;
    let permit = match gate().semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            enter_wait(kind, "admit", 0);
            let permit = match gate().semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    leave_wait(kind);
                    tracing::error!(?error, "object-store I/O semaphore closed unexpectedly");
                    return Permit { _permit: None };
                }
            };
            leave_wait(kind);
            permit
        }
    };
    wait_out_degradation(kind).await;
    Permit {
        _permit: Some(permit),
    }
}

fn enter_wait(kind: IoKind, reason: &'static str, wait_ms: u64) {
    let (delay_ms, failures) = {
        let Ok(mut state) = gate().state.lock() else {
            return;
        };
        state.last_kind = kind;
        state.active_waiters = state.active_waiters.saturating_add(1);
        (state.delay_ms, state.failures)
    };
    emit_throttle(ObjectStoreThrottleEvent::Enter {
        kind,
        reason,
        wait_ms,
        delay_ms,
        failures,
    });
}

fn leave_wait(kind: IoKind) {
    if let Ok(mut state) = gate().state.lock() {
        state.active_waiters = state.active_waiters.saturating_sub(1);
    }
    emit_throttle(ObjectStoreThrottleEvent::Leave { kind });
}

fn emit_update(kind: IoKind, reason: &'static str, wait_ms: u64) {
    let (delay_ms, failures) = {
        let Ok(state) = gate().state.lock() else {
            return;
        };
        (state.delay_ms, state.failures)
    };
    emit_throttle(ObjectStoreThrottleEvent::Update {
        kind,
        reason,
        wait_ms,
        delay_ms,
        failures,
    });
}

async fn wait_out_degradation(kind: IoKind) {
    let (sleep_for, delay_ms, failures) = {
        let Ok(state) = gate().state.lock() else {
            return;
        };
        let cooldown = state
            .cooldown_until
            .and_then(|until| until.checked_duration_since(Instant::now()))
            .unwrap_or_default();
        (cooldown, state.delay_ms, state.failures)
    };
    if sleep_for.is_zero() {
        return;
    }
    let wait_ms = sleep_for.as_millis() as u64;
    enter_wait(kind, "throttle", wait_ms);
    crate::store::index_build_progress::note(format!(
        "s3 {} throttle wait {:.1}s (failures={failures}, delay={delay_ms}ms)",
        kind.as_str(),
        sleep_for.as_secs_f32()
    ));
    tracing::warn!(
        target: "pchronicle.object_store_gate",
        kind = kind.as_str(),
        wait_ms,
        delay_ms,
        failures,
        "object-store I/O gate cooling down before next remote op"
    );
    // Tick the progress UI while cooling down so `cd=` counts down live.
    let deadline = Instant::now() + sleep_for;
    const TICK: Duration = Duration::from_millis(250);
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        emit_update(kind, "throttle", remaining.as_millis() as u64);
        tokio::time::sleep(remaining.min(TICK)).await;
    }
    leave_wait(kind);
}

/// Publish the current I/O phase for progress UI without taking a permit.
/// Used around Lance writes that do not go through [`acquire`].
pub(crate) fn mark_kind(kind: IoKind) {
    if let Ok(mut state) = gate().state.lock() {
        state.last_kind = kind;
    }
}

/// Record a successful remote op: decay shared delay after a streak.
pub(crate) fn note_success(uri: &str) {
    if !is_remote_uri(uri) {
        return;
    }
    let (kind, changed, delay_ms, failures) = {
        let Ok(mut state) = gate().state.lock() else {
            return;
        };
        let kind = state.last_kind;
        state.successes_since_backoff = state.successes_since_backoff.saturating_add(1);
        if state.delay_ms == 0 {
            return;
        }
        let mut changed = false;
        if state.successes_since_backoff >= SUCCESS_STREAK_TO_DECAY {
            state.delay_ms /= 2;
            state.successes_since_backoff = 0;
            if state.delay_ms < 100 {
                state.delay_ms = 0;
                state.cooldown_until = None;
            }
            changed = true;
            tracing::info!(
                target: "pchronicle.object_store_gate",
                delay_ms = state.delay_ms,
                "object-store I/O gate recovered toward steady state"
            );
        }
        (kind, changed, state.delay_ms, state.failures)
    };
    // Always publish streak / delay movement so the progress line can refresh.
    if !changed && delay_ms == 0 {
        // Healthy path: skip per-op UI spam; paints from commit/fetch cover s=.
        return;
    }
    let reason = if delay_ms == 0 { "recover" } else { "ok" };
    emit_throttle(ObjectStoreThrottleEvent::Update {
        kind,
        reason,
        wait_ms: 0,
        delay_ms,
        failures,
    });
}

/// Record a transient remote failure: grow shared delay and set a cooldown.
pub(crate) fn note_failure(uri: &str, kind: IoKind) {
    if !is_remote_uri(uri) {
        return;
    }
    let (delay_ms, failures, wait_ms) = {
        let Ok(mut state) = gate().state.lock() else {
            return;
        };
        state.last_kind = kind;
        state.failures = state.failures.saturating_add(1);
        state.successes_since_backoff = 0;
        state.delay_ms = if state.delay_ms == 0 {
            500
        } else {
            state.delay_ms.saturating_mul(2).min(MAX_DELAY_MS)
        };
        state.cooldown_until = Some(Instant::now() + Duration::from_millis(state.delay_ms));
        tracing::warn!(
            target: "pchronicle.object_store_gate",
            kind = kind.as_str(),
            delay_ms = state.delay_ms,
            failures = state.failures,
            "object-store I/O gate backing off after transient failure"
        );
        crate::store::index_build_progress::note(format!(
            "s3 {} throttle backoff {}ms",
            kind.as_str(),
            state.delay_ms
        ));
        (state.delay_ms, state.failures, state.delay_ms)
    };
    emit_throttle(ObjectStoreThrottleEvent::Update {
        kind,
        reason: "backoff",
        wait_ms,
        delay_ms,
        failures,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aimd_flow_label_healthy_and_degraded() {
        let healthy = ObjectStoreGateSnapshot {
            kind: IoKind::Write,
            delay_ms: 0,
            cooldown_remaining_ms: 0,
            failures: 0,
            success_streak: 2,
            success_streak_target: SUCCESS_STREAK_TO_DECAY,
            active_waiters: 0,
            available_permits: 1,
            max_permits: 1,
        };
        assert_eq!(
            format_aimd_flow_label(&healthy, None),
            "aimd ok s=2/4 p=1/1"
        );

        let cooling = ObjectStoreGateSnapshot {
            delay_ms: 2000,
            cooldown_remaining_ms: 1500,
            failures: 3,
            success_streak: 0,
            active_waiters: 1,
            available_permits: 0,
            ..healthy
        };
        let label = format_aimd_flow_label(&cooling, Some("throttle"));
        assert!(label.starts_with("aimd throttle "), "{label}");
        assert!(label.contains("cd=1.5s"), "{label}");
        assert!(label.contains("d=2000ms"), "{label}");
        assert!(label.contains("f=3"), "{label}");
        assert!(label.contains("w=1"), "{label}");
        assert!(label.contains("p=0/1"), "{label}");
    }
}
