//! Observable interception coverage for OverlayNet drivers.
//!
//! Coverage is deliberately separate from egress policy. A deny decision is
//! only an enforcement guarantee when the active driver is non-bypassable.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InterceptionDriver {
    ExplicitProxy,
    LinuxNetns,
    LinuxSeccompNotify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InterceptionStrength {
    /// Only traffic that cooperates with the configured proxy is observed.
    Cooperative,
    /// The process tree has no network path around the driver.
    NonBypassable,
}

/// Stable description of what an active interception driver actually covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptionProfile {
    pub driver: InterceptionDriver,
    pub strength: InterceptionStrength,
    pub http: bool,
    pub tcp_connect: bool,
    pub dns: bool,
    pub udp: bool,
    pub inherited_by_children: bool,
}

impl InterceptionProfile {
    pub const fn explicit_proxy() -> Self {
        Self {
            driver: InterceptionDriver::ExplicitProxy,
            strength: InterceptionStrength::Cooperative,
            http: true,
            tcp_connect: true,
            dns: false,
            udp: false,
            inherited_by_children: false,
        }
    }

    pub const fn is_enforcing(&self) -> bool {
        matches!(self.strength, InterceptionStrength::NonBypassable)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptionSnapshot {
    pub requests_seen: u64,
    pub policy_allowed: u64,
    pub policy_denied: u64,
    pub connect_requests: u64,
    pub absolute_http_requests: u64,
    pub sink_requests: u64,
    pub failures: u64,
}

#[derive(Default)]
struct Counters {
    requests_seen: AtomicU64,
    policy_allowed: AtomicU64,
    policy_denied: AtomicU64,
    connect_requests: AtomicU64,
    absolute_http_requests: AtomicU64,
    sink_requests: AtomicU64,
    failures: AtomicU64,
}

/// Cloneable, lock-free counters for the traffic that reached OverlayNet.
///
/// These counters measure intercepted traffic, not traffic that bypassed a
/// cooperative driver. The accompanying [`InterceptionProfile`] carries that
/// distinction.
#[derive(Clone, Default)]
pub struct InterceptionMetrics {
    counters: Arc<Counters>,
}

impl InterceptionMetrics {
    pub fn snapshot(&self) -> InterceptionSnapshot {
        InterceptionSnapshot {
            requests_seen: self.counters.requests_seen.load(Ordering::Relaxed),
            policy_allowed: self.counters.policy_allowed.load(Ordering::Relaxed),
            policy_denied: self.counters.policy_denied.load(Ordering::Relaxed),
            connect_requests: self.counters.connect_requests.load(Ordering::Relaxed),
            absolute_http_requests: self.counters.absolute_http_requests.load(Ordering::Relaxed),
            sink_requests: self.counters.sink_requests.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn request_seen(&self) {
        self.counters.requests_seen.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn policy_allowed(&self) {
        self.counters.policy_allowed.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn policy_denied(&self) {
        self.counters.policy_denied.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn connect_request(&self) {
        self.counters
            .connect_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn absolute_http_request(&self) {
        self.counters
            .absolute_http_requests
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn sink_request(&self) {
        self.counters.sink_requests.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn failure(&self) {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_proxy_never_claims_non_bypassable_enforcement() {
        let profile = InterceptionProfile::explicit_proxy();
        assert_eq!(profile.strength, InterceptionStrength::Cooperative);
        assert!(!profile.is_enforcing());
        assert!(!profile.dns);
        assert!(!profile.udp);
    }

    #[test]
    fn metrics_snapshots_are_shared_across_clones() {
        let metrics = InterceptionMetrics::default();
        let clone = metrics.clone();
        metrics.request_seen();
        clone.policy_denied();

        assert_eq!(
            metrics.snapshot(),
            InterceptionSnapshot {
                requests_seen: 1,
                policy_denied: 1,
                ..InterceptionSnapshot::default()
            }
        );
    }

    #[test]
    fn every_metric_updates_only_its_own_counter() {
        let metrics = InterceptionMetrics::default();
        metrics.request_seen();
        metrics.policy_allowed();
        metrics.policy_denied();
        metrics.connect_request();
        metrics.absolute_http_request();
        metrics.sink_request();
        metrics.failure();
        assert_eq!(
            metrics.snapshot(),
            InterceptionSnapshot {
                requests_seen: 1,
                policy_allowed: 1,
                policy_denied: 1,
                connect_requests: 1,
                absolute_http_requests: 1,
                sink_requests: 1,
                failures: 1,
            }
        );
    }

    #[test]
    fn concurrent_metric_updates_are_not_lost() {
        let metrics = InterceptionMetrics::default();
        let workers = (0..8)
            .map(|_| {
                let metrics = metrics.clone();
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        metrics.request_seen();
                        metrics.policy_allowed();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests_seen, 80_000);
        assert_eq!(snapshot.policy_allowed, 80_000);
        assert_eq!(snapshot.policy_denied, 0);
    }
}
