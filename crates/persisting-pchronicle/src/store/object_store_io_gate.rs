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
pub(crate) enum IoKind {
    Read,
    Write,
}

impl IoKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
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
}

impl Default for AimdState {
    fn default() -> Self {
        Self {
            delay_ms: 0,
            cooldown_until: None,
            successes_since_backoff: 0,
            failures: 0,
            last_kind: IoKind::Read,
        }
    }
}

struct Gate {
    semaphore: Arc<Semaphore>,
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
    let permit = gate()
        .semaphore
        .clone()
        .acquire_owned()
        .await
        .expect("object-store I/O semaphore is never closed");
    wait_out_degradation(kind).await;
    Permit {
        _permit: Some(permit),
    }
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
    crate::store::index_build_progress::note(format!(
        "s3 {} throttle wait {:.1}s (failures={failures}, delay={delay_ms}ms)",
        kind.as_str(),
        sleep_for.as_secs_f32()
    ));
    tracing::warn!(
        target: "pchronicle.object_store_gate",
        kind = kind.as_str(),
        wait_ms = sleep_for.as_millis() as u64,
        delay_ms,
        failures,
        "object-store I/O gate cooling down before next remote op"
    );
    tokio::time::sleep(sleep_for).await;
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
    let Ok(mut state) = gate().state.lock() else {
        return;
    };
    state.successes_since_backoff = state.successes_since_backoff.saturating_add(1);
    if state.delay_ms == 0 {
        return;
    }
    if state.successes_since_backoff >= SUCCESS_STREAK_TO_DECAY {
        state.delay_ms /= 2;
        state.successes_since_backoff = 0;
        if state.delay_ms < 100 {
            state.delay_ms = 0;
            state.cooldown_until = None;
        }
        tracing::info!(
            target: "pchronicle.object_store_gate",
            delay_ms = state.delay_ms,
            "object-store I/O gate recovered toward steady state"
        );
    }
}

/// Record a transient remote failure: grow shared delay and set a cooldown.
pub(crate) fn note_failure(uri: &str, kind: IoKind) {
    if !is_remote_uri(uri) {
        return;
    }
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
}

#[cfg(test)]
pub(crate) fn debug_delay_ms() -> u64 {
    gate()
        .state
        .lock()
        .map(|state| state.delay_ms)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_remote_uris() {
        assert!(is_remote_uri("s3://bucket/prefix"));
        assert!(is_remote_uri("gs://bucket/prefix"));
        assert!(!is_remote_uri("/tmp/local"));
        assert!(!is_remote_uri("file:///tmp/local"));
        assert!(!is_remote_uri("shared-memory://x"));
    }
}
