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
    VmSmoltcp,
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

    pub const fn vm_smoltcp() -> Self {
        Self {
            driver: InterceptionDriver::VmSmoltcp,
            strength: InterceptionStrength::NonBypassable,
            http: false,
            tcp_connect: true,
            dns: true,
            udp: false,
            inherited_by_children: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterceptionSnapshot {
    pub requests_seen: u64,
    pub policy_allowed: u64,
    pub policy_denied: u64,
    pub connect_requests: u64,
    pub absolute_http_requests: u64,
    pub sink_requests: u64,
    pub failures: u64,
    pub dns_queries: u64,
    pub dns_answers: u64,
    pub tcp_flows_opened: u64,
    pub tcp_flows_denied: u64,
    pub tcp_connect_failures: u64,
    pub bytes_guest_to_host: u64,
    pub bytes_host_to_guest: u64,
    pub unsupported_packets: u64,
    pub active_tcp_flows: u64,
    pub peak_tcp_flows: u64,
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
    dns_queries: AtomicU64,
    dns_answers: AtomicU64,
    tcp_flows_opened: AtomicU64,
    tcp_flows_denied: AtomicU64,
    tcp_connect_failures: AtomicU64,
    bytes_guest_to_host: AtomicU64,
    bytes_host_to_guest: AtomicU64,
    unsupported_packets: AtomicU64,
    active_tcp_flows: AtomicU64,
    peak_tcp_flows: AtomicU64,
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
            dns_queries: self.counters.dns_queries.load(Ordering::Relaxed),
            dns_answers: self.counters.dns_answers.load(Ordering::Relaxed),
            tcp_flows_opened: self.counters.tcp_flows_opened.load(Ordering::Relaxed),
            tcp_flows_denied: self.counters.tcp_flows_denied.load(Ordering::Relaxed),
            tcp_connect_failures: self.counters.tcp_connect_failures.load(Ordering::Relaxed),
            bytes_guest_to_host: self.counters.bytes_guest_to_host.load(Ordering::Relaxed),
            bytes_host_to_guest: self.counters.bytes_host_to_guest.load(Ordering::Relaxed),
            unsupported_packets: self.counters.unsupported_packets.load(Ordering::Relaxed),
            active_tcp_flows: self.counters.active_tcp_flows.load(Ordering::Relaxed),
            peak_tcp_flows: self.counters.peak_tcp_flows.load(Ordering::Relaxed),
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

    pub(crate) fn dns_query(&self) {
        self.counters.dns_queries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn dns_answer(&self) {
        self.counters.dns_answers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn tcp_flow_opened(&self) {
        self.counters
            .tcp_flows_opened
            .fetch_add(1, Ordering::Relaxed);
        let active = self
            .counters
            .active_tcp_flows
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        self.counters
            .peak_tcp_flows
            .fetch_max(active, Ordering::Relaxed);
    }

    pub(crate) fn tcp_flow_closed(&self) {
        let _ = self.counters.active_tcp_flows.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |active| active.checked_sub(1),
        );
    }

    pub(crate) fn tcp_flow_denied(&self) {
        self.counters
            .tcp_flows_denied
            .fetch_add(1, Ordering::Relaxed);
        self.policy_denied();
    }

    pub(crate) fn tcp_connect_failure(&self) {
        self.counters
            .tcp_connect_failures
            .fetch_add(1, Ordering::Relaxed);
        self.failure();
    }

    pub(crate) fn guest_to_host(&self, bytes: usize) {
        self.counters
            .bytes_guest_to_host
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn host_to_guest(&self, bytes: usize) {
        self.counters
            .bytes_host_to_guest
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn unsupported_packet(&self) {
        self.counters
            .unsupported_packets
            .fetch_add(1, Ordering::Relaxed);
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
    fn vm_smoltcp_claims_only_implemented_protocols() {
        let profile = InterceptionProfile::vm_smoltcp();
        assert!(profile.is_enforcing());
        assert!(profile.tcp_connect);
        assert!(profile.dns);
        assert!(profile.inherited_by_children);
        assert!(!profile.http);
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
                ..InterceptionSnapshot::default()
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
