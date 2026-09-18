//! Process-wide admission + AIMD backoff for remote object-store I/O.
//!
//! Lance opens and table writes against flaky S3-compatible gateways amplify
//! timeouts when several datasets race (list `_versions/`, retries, AIMD inside
//! object_store). This gate:
//! 1. caps concurrent remote ops per endpoint + bucket (default 4);
//! 2. after a transient failure, forces a shared cooldown + growing delay;
//! 3. decays the delay after a streak of successes;
//! 4. keeps interactive (foreground) work ahead of browse/maintenance
//!    (background): background acquires yield while the same scope has
//!    foreground demand, so shared S3 bandwidth is not split evenly.
//!
//! Local `file://` paths bypass the gate entirely.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const DEFAULT_REMOTE_CONCURRENCY: usize = 4;
const DEFAULT_BACKGROUND_CONCURRENCY: usize = 1;
const MAX_REMOTE_CONCURRENCY: usize = 8;
const MAX_RETAINED_SCOPES: usize = 1024;
const SCOPE_IDLE_TTL: Duration = Duration::from_secs(300);
const MAX_DELAY_MS: u64 = 30_000;
const SUCCESS_STREAK_TO_DECAY: u32 = 4;
const BACKGROUND_YIELD_POLL: Duration = Duration::from_millis(25);

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

#[derive(Debug, Clone)]
struct AimdState {
    semaphore: Arc<Semaphore>,
    last_used: Instant,
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
            semaphore: Arc::new(Semaphore::new(DEFAULT_REMOTE_CONCURRENCY)),
            last_used: Instant::now(),
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
    /// `foreground` or `background`, so a log line says which lane queued.
    lane: &'static str,
    concurrency: usize,
    states: Mutex<HashMap<String, AimdState>>,
}

/// Admission waits shorter than this are noise next to a remote round trip.
const WAIT_LOG_THRESHOLD: Duration = Duration::from_millis(100);

fn state_for<'a>(
    states: &'a mut HashMap<String, AimdState>,
    key: &str,
    concurrency: usize,
) -> &'a mut AimdState {
    let now = Instant::now();
    if !states.contains_key(key) {
        states.retain(|_, state| {
            Arc::strong_count(&state.semaphore) > 1
                || state.active_waiters > 0
                || state.cooldown_until.is_some_and(|until| until > now)
                || now.duration_since(state.last_used) < SCOPE_IDLE_TTL
        });
        while states.len() >= MAX_RETAINED_SCOPES {
            let oldest = states
                .iter()
                .filter(|(_, state)| {
                    Arc::strong_count(&state.semaphore) == 1
                        && state.active_waiters == 0
                        && state.cooldown_until.is_none_or(|until| until <= now)
                })
                .min_by_key(|(_, state)| state.last_used)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                states.remove(&oldest);
            } else {
                break;
            }
        }
    }
    // Live waits/cooldowns may temporarily exceed the retention limit. Evicting
    // them would let backend overload bypass AIMD; reclaim after they finish.
    let state = states.entry(key.to_owned()).or_insert_with(|| AimdState {
        semaphore: Arc::new(Semaphore::new(concurrency)),
        ..Default::default()
    });
    state.last_used = now;
    state
}

tokio::task_local! {
    static BACKGROUND_IO: ();
}

/// Run manifest maintenance with independent admission and AIMD state. Call
/// inside the spawned task: Tokio task-local state is not inherited by spawn.
pub async fn with_background_object_store_io<F: std::future::Future>(work: F) -> F::Output {
    BACKGROUND_IO.scope((), work).await
}

/// Process-wide interactive demand per admission scope (holders + in-flight
/// acquires). Background work polls this so browse refresh does not share the
/// pipe with turns/run while a user request is active.
fn foreground_demand_map() -> &'static Mutex<HashMap<String, u32>> {
    static FOREGROUND_DEMAND: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    FOREGROUND_DEMAND.get_or_init(|| Mutex::new(HashMap::new()))
}

fn enter_foreground_demand(key: &str) {
    let Ok(mut map) = foreground_demand_map().lock() else {
        return;
    };
    *map.entry(key.to_owned()).or_insert(0) += 1;
}

fn leave_foreground_demand(key: &str) {
    let Ok(mut map) = foreground_demand_map().lock() else {
        return;
    };
    let Some(count) = map.get_mut(key) else {
        return;
    };
    *count = count.saturating_sub(1);
    if *count == 0 {
        map.remove(key);
    }
}

fn scope_foreground_demand(key: &str) -> u32 {
    foreground_demand_map()
        .lock()
        .ok()
        .and_then(|map| map.get(key).copied())
        .unwrap_or(0)
}

/// Total interactive object-store acquires currently in flight (any scope).
pub fn foreground_object_store_demand() -> u32 {
    foreground_demand_map()
        .lock()
        .ok()
        .map(|map| map.values().copied().sum())
        .unwrap_or(0)
}

/// Block until no interactive object-store work is admitted. Browse refresh
/// calls this before starting a background walk so user requests go first.
pub async fn wait_for_foreground_object_store_idle() {
    let mut logged = false;
    let started = Instant::now();
    loop {
        if foreground_object_store_demand() == 0 {
            if logged {
                tracing::debug!(
                    target: "pchronicle.object_store_gate",
                    yielded_ms = started.elapsed().as_millis() as u64,
                    "background resumed after interactive object-store idle"
                );
            }
            return;
        }
        if !logged {
            tracing::debug!(
                target: "pchronicle.object_store_gate",
                demand = foreground_object_store_demand(),
                "background yielding to interactive object-store demand"
            );
            logged = true;
        }
        tokio::time::sleep(BACKGROUND_YIELD_POLL).await;
    }
}

struct ForegroundDemandGuard {
    key: String,
}

impl ForegroundDemandGuard {
    fn enter(key: &str) -> Self {
        enter_foreground_demand(key);
        Self {
            key: key.to_owned(),
        }
    }
}

impl Drop for ForegroundDemandGuard {
    fn drop(&mut self) {
        leave_foreground_demand(&self.key);
    }
}

fn gate() -> &'static Gate {
    static FOREGROUND_GATE: OnceLock<Gate> = OnceLock::new();
    static BACKGROUND_GATE: OnceLock<Gate> = OnceLock::new();
    let background = BACKGROUND_IO.try_with(|_| ()).is_ok();
    let slot = if background {
        &BACKGROUND_GATE
    } else {
        &FOREGROUND_GATE
    };
    slot.get_or_init(|| {
        let configured = std::env::var("PCHRONICLE_OBJECT_STORE_CONCURRENCY")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_REMOTE_CONCURRENCY)
            .clamp(1, MAX_REMOTE_CONCURRENCY);
        // Background stays single-flight: even when interactive is idle, browse
        // should not open a second S3 pipeline beside itself.
        let concurrency = if background {
            DEFAULT_BACKGROUND_CONCURRENCY.min(configured)
        } else {
            configured
        };
        Gate {
            lane: if background {
                "background"
            } else {
                "foreground"
            },
            concurrency,
            states: Mutex::new(HashMap::new()),
        }
    })
}

/// Canonical admission scope, independent of object path and cache credentials.
/// Adapters capture this at construction so later environment changes cannot
/// move feedback for an existing store into another endpoint's state.
pub(crate) fn scope_key(uri: &str) -> String {
    if uri
        .split_once('#')
        .is_some_and(|(_, fragment)| fragment.starts_with("endpoint="))
    {
        return uri.to_owned();
    }
    let endpoint_vars: &[&str] = if uri.starts_with("s3") {
        &["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT", "AWS_ENDPOINT_URL"]
    } else if uri.starts_with("az") {
        &["AZURE_STORAGE_ENDPOINT"]
    } else {
        &["GOOGLE_STORAGE_BASE_URL"]
    };
    let endpoint = endpoint_vars
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    scope_for_endpoint(uri, &endpoint)
}

pub(crate) fn scope_for_endpoint(uri: &str, endpoint: &str) -> String {
    let uri = uri.split('#').next().unwrap_or(uri);
    let Some((scheme, rest)) = uri.split_once("://") else {
        return uri.to_owned();
    };
    let bucket = rest.split('/').next().unwrap_or(rest);
    let endpoint = url::Url::parse(endpoint)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| endpoint.to_owned());
    let identity = blake3::hash(endpoint.trim_end_matches('/').as_bytes());
    format!("{scheme}://{bucket}#endpoint={}", identity.to_hex())
}

/// True for s3/gs/az (and similar) URIs; false for local paths / file://.
pub(crate) fn is_remote_uri(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once("://") else {
        return false;
    };
    !matches!(scheme, "file" | "file+uring" | "memory" | "shared-memory")
}

/// Classify errors for shared AIMD feedback. Permanent object identity,
/// authorization, and precondition failures must never increase cooldown.
pub(crate) fn is_transient_error(error: &object_store::Error) -> bool {
    if matches!(
        error,
        object_store::Error::NotFound { .. }
            | object_store::Error::InvalidPath { .. }
            | object_store::Error::NotSupported { .. }
            | object_store::Error::AlreadyExists { .. }
            | object_store::Error::Precondition { .. }
            | object_store::Error::NotModified { .. }
            | object_store::Error::PermissionDenied { .. }
            | object_store::Error::Unauthenticated { .. }
    ) {
        return false;
    }
    let text = error.to_string().to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "connection",
        "broken pipe",
        "temporarily",
        "slowdown",
        "throttl",
        "503",
        "429",
        "reset",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// Snapshot AIMD / cooldown state for progress UI.
pub fn snapshot() -> ObjectStoreGateSnapshot {
    let g = gate();
    let available_permits = g.concurrency;
    let max_permits = g.concurrency;
    let Ok(states) = g.states.lock() else {
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
    let state = states.values().max_by_key(|state| state.failures);
    let Some(state) = state else {
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
        available_permits: state.semaphore.available_permits(),
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
    /// Held for the full foreground acquire+hold window so background yields.
    _demand: Option<ForegroundDemandGuard>,
}

/// Acquire admission for a Lance/object-store operation on `uri`.
pub(crate) async fn acquire(uri: &str, kind: IoKind) -> Permit {
    if !is_remote_uri(uri) {
        return Permit {
            _permit: None,
            _demand: None,
        };
    }
    acquire_scoped(gate(), &scope_key(uri), kind).await
}

async fn yield_to_foreground(key: &str, kind: IoKind) {
    let mut logged = false;
    let started = Instant::now();
    loop {
        if scope_foreground_demand(key) == 0 {
            if logged {
                tracing::debug!(
                    target: "pchronicle.object_store_gate",
                    scope = key,
                    kind = kind.as_str(),
                    yielded_ms = started.elapsed().as_millis() as u64,
                    "background admission resumed after interactive demand cleared"
                );
            }
            return;
        }
        if !logged {
            tracing::debug!(
                target: "pchronicle.object_store_gate",
                scope = key,
                kind = kind.as_str(),
                demand = scope_foreground_demand(key),
                "background admission yielding to interactive object-store demand"
            );
            logged = true;
        }
        tokio::time::sleep(BACKGROUND_YIELD_POLL).await;
    }
}

async fn acquire_scoped(g: &Gate, key: &str, kind: IoKind) -> Permit {
    let background = g.lane == "background";
    // Interactive demand covers the whole wait+hold window so browse cannot
    // race into the same S3 endpoint between admit and first byte.
    let demand = if background {
        None
    } else {
        Some(ForegroundDemandGuard::enter(key))
    };
    let semaphore = {
        let mut states = g
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = state_for(&mut states, key, g.concurrency);
        state.last_kind = kind;
        Arc::clone(&state.semaphore)
    };
    loop {
        if background {
            yield_to_foreground(key, kind).await;
        }
        wait_out_degradation(g, key, kind).await;
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                // A stall here is admission queueing, not the backend. Without
                // a log line the two are indistinguishable in a trace, and
                // every remote read looks like a slow round trip.
                let queued = Instant::now();
                let _wait = WaitGuard::new(g, key, kind, "admit", 0);
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(error) => {
                        tracing::error!(?error, "object-store I/O semaphore closed unexpectedly");
                        return Permit {
                            _permit: None,
                            _demand: demand,
                        };
                    }
                };
                let waited = queued.elapsed();
                if waited >= WAIT_LOG_THRESHOLD {
                    tracing::debug!(
                        lane = g.lane,
                        scope = key,
                        kind = kind.as_str(),
                        waited_ms = waited.as_millis() as u64,
                        concurrency = g.concurrency,
                        "object-store admission queued"
                    );
                }
                permit
            }
        };
        // A failure may have started a new cooldown while admission was
        // queued. Release capacity before sleeping, then compete again.
        if !cooldown_remaining(g, key).is_zero() {
            drop(permit);
            continue;
        }
        // Interactive arrived while we waited for a background slot: give the
        // permit back and yield instead of holding endpoint bandwidth.
        if background && scope_foreground_demand(key) > 0 {
            drop(permit);
            continue;
        }
        return Permit {
            _permit: Some(permit),
            _demand: demand,
        };
    }
}

fn cooldown_remaining(g: &Gate, key: &str) -> Duration {
    g.states
        .lock()
        .ok()
        .and_then(|states| states.get(key).and_then(|state| state.cooldown_until))
        .and_then(|until| until.checked_duration_since(Instant::now()))
        .unwrap_or_default()
}

// Cancellation of an HTTP request must also release progress wait accounting.
struct WaitGuard<'a> {
    gate: &'a Gate,
    key: &'a str,
    kind: IoKind,
}
impl<'a> WaitGuard<'a> {
    fn new(gate: &'a Gate, key: &'a str, kind: IoKind, reason: &'static str, wait_ms: u64) -> Self {
        enter_wait(gate, key, kind, reason, wait_ms);
        Self { gate, key, kind }
    }
}
impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        leave_wait(self.gate, self.key, self.kind);
    }
}

fn enter_wait(g: &Gate, key: &str, kind: IoKind, reason: &'static str, wait_ms: u64) {
    let (delay_ms, failures) = {
        let Ok(mut states) = g.states.lock() else {
            return;
        };
        let state = state_for(&mut states, key, g.concurrency);
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

fn leave_wait(g: &Gate, key: &str, kind: IoKind) {
    if let Ok(mut states) = g.states.lock() {
        let state = state_for(&mut states, key, g.concurrency);
        state.active_waiters = state.active_waiters.saturating_sub(1);
    }
    emit_throttle(ObjectStoreThrottleEvent::Leave { kind });
}

fn emit_update(g: &Gate, key: &str, kind: IoKind, reason: &'static str, wait_ms: u64) {
    let (delay_ms, failures) = {
        let Ok(states) = g.states.lock() else {
            return;
        };
        let state = states.get(key).cloned().unwrap_or_default();
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

async fn wait_out_degradation(g: &Gate, key: &str, kind: IoKind) {
    let (sleep_for, delay_ms, failures) = {
        let Ok(states) = g.states.lock() else {
            return;
        };
        let Some(state) = states.get(key) else {
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
    let _wait = WaitGuard::new(g, key, kind, "throttle", wait_ms);
    crate::store::index_build_progress::note(format!(
        "s3 {} throttle wait {:.1}s (failures={failures}, delay={delay_ms}ms)",
        kind.as_str(),
        sleep_for.as_secs_f32()
    ));
    // A cooldown can have several queued callers; the failure itself is
    // already logged by `note_failure`, so each waiter need not be WARN noise.
    tracing::debug!(
        target: "pchronicle.object_store_gate",
        lane = g.lane,
        kind = kind.as_str(),
        wait_ms,
        delay_ms,
        failures,
        "object-store I/O gate cooling down before next remote op"
    );
    // Tick the progress UI while cooling down so `cd=` counts down live.
    const TICK: Duration = Duration::from_millis(250);
    loop {
        let remaining = cooldown_remaining(g, key);
        if remaining.is_zero() {
            break;
        }
        emit_update(g, key, kind, "throttle", remaining.as_millis() as u64);
        tokio::time::sleep(remaining.min(TICK)).await;
    }
}

/// Publish the current I/O phase for progress UI without taking a permit.
/// Used around Lance writes that do not go through [`acquire`].
pub(crate) fn mark_kind(uri: &str, kind: IoKind) {
    if !is_remote_uri(uri) {
        return;
    }
    if let Ok(mut states) = gate().states.lock() {
        let state = state_for(&mut states, &scope_key(uri), gate().concurrency);
        state.last_kind = kind;
    }
}

/// Record a successful remote op: decay shared delay after a streak.
pub(crate) fn note_success(uri: &str) {
    if !is_remote_uri(uri) {
        return;
    }
    let (kind, changed, delay_ms, failures) = {
        let key = scope_key(uri);
        let Ok(mut states) = gate().states.lock() else {
            return;
        };
        let state = state_for(&mut states, &key, gate().concurrency);
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
        let key = scope_key(uri);
        let Ok(mut states) = gate().states.lock() else {
            return;
        };
        let state = state_for(&mut states, &key, gate().concurrency);
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
            scope = %key,
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

    #[tokio::test]
    async fn background_yields_while_foreground_holds_admission() {
        let uri = scope_for_endpoint("s3://workload-isolation/path", "http://localhost:18060");
        let mut foreground_permits = Vec::new();
        for _ in 0..gate().concurrency {
            foreground_permits.push(acquire(&uri, IoKind::Read).await);
        }
        assert!(foreground_object_store_demand() > 0);
        with_background_object_store_io(async {
            let raced =
                tokio::time::timeout(Duration::from_millis(80), acquire(&uri, IoKind::Read)).await;
            assert!(
                raced.is_err(),
                "background must yield while interactive holds the endpoint"
            );
        })
        .await;
        drop(foreground_permits);
        assert_eq!(foreground_object_store_demand(), 0);
        with_background_object_store_io(async {
            let _permit =
                tokio::time::timeout(Duration::from_millis(200), acquire(&uri, IoKind::Read))
                    .await
                    .expect("background proceeds once interactive is idle");
            note_failure(&uri, IoKind::Read);
            assert_eq!(gate().states.lock().unwrap()[&uri].failures, 1);
        })
        .await;
        // Foreground AIMD is independent of the background failure above.
        assert_eq!(
            gate()
                .states
                .lock()
                .unwrap()
                .get(&uri)
                .map(|s| s.failures)
                .unwrap_or(0),
            0
        );
        let _permit = tokio::time::timeout(Duration::from_millis(100), acquire(&uri, IoKind::Read))
            .await
            .expect("background cooldown must not delay foreground requests");
        note_success(&uri);
        with_background_object_store_io(async {
            assert_eq!(gate().states.lock().unwrap()[&uri].failures, 1);
        })
        .await;
    }

    #[tokio::test]
    async fn foreground_and_background_have_independent_admission_and_feedback() {
        let uri = scope_for_endpoint("s3://aimd-isolation/path", "http://localhost:18061");
        // Idle interactive: background may admit on its own lane immediately.
        with_background_object_store_io(async {
            let _permit =
                tokio::time::timeout(Duration::from_millis(100), acquire(&uri, IoKind::Read))
                    .await
                    .expect("background must not wait when interactive is idle");
            note_failure(&uri, IoKind::Read);
            assert_eq!(gate().states.lock().unwrap()[&uri].failures, 1);
        })
        .await;
        assert_eq!(
            gate().states.lock().unwrap().get(&uri).map(|s| s.failures),
            None
        );
        let _permit = tokio::time::timeout(Duration::from_millis(100), acquire(&uri, IoKind::Read))
            .await
            .expect("background cooldown must not delay foreground requests");
        note_success(&uri);
        with_background_object_store_io(async {
            assert_eq!(gate().states.lock().unwrap()[&uri].failures, 1);
        })
        .await;
    }

    #[test]
    fn registry_reclaims_idle_scopes_but_preserves_waits_and_cooldowns() {
        let mut states = HashMap::new();
        state_for(&mut states, "waiting", 1).active_waiters = 1;
        state_for(&mut states, "cooling", 1).cooldown_until =
            Some(Instant::now() + Duration::from_secs(60));
        for n in 0..MAX_RETAINED_SCOPES * 2 {
            state_for(&mut states, &n.to_string(), 1);
        }
        assert_eq!(states.len(), MAX_RETAINED_SCOPES);
        assert!(states.contains_key("waiting"));
        assert!(states.contains_key("cooling"));
        assert!(!states.contains_key("0"));
        for state in states.values_mut() {
            state.last_used = Instant::now() - SCOPE_IDLE_TTL;
        }
        state_for(&mut states, "new", 1);
        assert_eq!(states.len(), 3);
    }

    #[test]
    fn scopes_share_bucket_paths_and_isolate_endpoints_and_buckets() {
        let scope = scope_for_endpoint(
            "s3://bucket/a/table#old-cache-credentials",
            "HTTP://LOCALHOST:9000/",
        );
        assert_eq!(
            scope,
            scope_for_endpoint("s3://bucket/b", "http://localhost:9000")
        );
        assert_eq!(scope_key(&scope), scope);
        assert_ne!(
            scope,
            scope_for_endpoint("s3://bucket/a", "http://localhost:9001")
        );
        assert_ne!(
            scope,
            scope_for_endpoint("s3://other/a", "http://localhost:9000")
        );
    }

    #[tokio::test]
    async fn busy_backend_does_not_block_other_endpoints_or_buckets() {
        let g = Gate {
            lane: "foreground",
            concurrency: 1,
            states: Mutex::new(HashMap::new()),
        };
        let busy = scope_for_endpoint("s3://bucket/a", "http://slow");
        let held = acquire_scoped(&g, &busy, IoKind::Read).await;
        for independent in [
            scope_for_endpoint("s3://bucket/a", "http://healthy"),
            scope_for_endpoint("s3://other/a", "http://slow"),
        ] {
            let permit = tokio::time::timeout(
                Duration::from_millis(200),
                acquire_scoped(&g, &independent, IoKind::Read),
            )
            .await
            .unwrap();
            drop(permit);
        }
        // Registry reclamation must not replace a semaphore with a live permit.
        {
            let mut states = g.states.lock().unwrap();
            states.get_mut(&busy).unwrap().last_used = Instant::now() - SCOPE_IDLE_TTL;
            for n in 0..MAX_RETAINED_SCOPES + 1 {
                state_for(&mut states, &n.to_string(), g.concurrency);
            }
            assert!(states.contains_key(&busy));
        }
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                acquire_scoped(&g, &busy, IoKind::Read)
            )
            .await
            .is_err()
        );
        assert_eq!(g.states.lock().unwrap()[&busy].active_waiters, 0);
        drop(held);
        let _permit = tokio::time::timeout(
            Duration::from_millis(200),
            acquire_scoped(&g, &busy, IoKind::Read),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cooldown_after_queued_admission_releases_capacity_and_cancellation_clears_waiters() {
        let g = Arc::new(Gate {
            lane: "foreground",
            concurrency: 1,
            states: Mutex::new(HashMap::new()),
        });
        let bad = scope_for_endpoint("s3://bucket/a", "http://slow");
        let healthy = scope_for_endpoint("s3://bucket/a", "http://healthy");
        let held = acquire_scoped(&g, &bad, IoKind::Read).await;
        let waiter = tokio::spawn({
            let g = g.clone();
            let bad = bad.clone();
            async move { acquire_scoped(&g, &bad, IoKind::Read).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while g
                .states
                .lock()
                .unwrap()
                .get(&bad)
                .is_none_or(|s| s.active_waiters == 0)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        g.states
            .lock()
            .unwrap()
            .get_mut(&bad)
            .unwrap()
            .cooldown_until = Some(Instant::now() + Duration::from_secs(10));
        drop(held);
        // Cooling on one endpoint must not block another endpoint.
        let permit = tokio::time::timeout(
            Duration::from_millis(500),
            acquire_scoped(&g, &healthy, IoKind::Read),
        )
        .await
        .unwrap();
        assert!(!waiter.is_finished());
        waiter.abort();
        assert!(waiter.await.is_err());
        assert_eq!(g.states.lock().unwrap()[&bad].active_waiters, 0);
        drop(permit);
        assert_eq!(
            g.states.lock().unwrap()[&healthy]
                .semaphore
                .available_permits(),
            1
        );
    }

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
