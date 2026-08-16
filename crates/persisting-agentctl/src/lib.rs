//! Shared Agent control contracts, policies, wire protocol, and client SDK.
//!
//! Drivers such as OverlayNet and Capture submit typed resources to a
//! [`ControlController`]. Authorization is represented as a state transition;
//! the driver then records whether the authorized operation was applied or
//! failed. [`AgentCtlClient`] implements the Run-scoped Unix Agent ABI, while
//! [`PVisorProcessClient`] submits a [`RunSpec`] to a standalone foreground
//! pVisor binary. Supervisor messages are shared wire contracts rather than
//! types owned by either pPilot or pVisor.

pub mod abi;
mod client;
mod process;
mod runtime;
mod supervisor;

pub use abi::*;
pub use client::{checkpoint_directive, AgentCtlClient, AgentCtlClientConfig};
use ipnet::IpNet;
pub use process::{PVisorProcessClient, PVisorProcessOptions};
pub use runtime::*;
pub use runtime::{AccessEffect as ControlEffect, AccessReason as ControlReason};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::str::FromStr;
pub use supervisor::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    Requested,
    Allowed,
    Denied,
    Applied { effect: ControlEffect },
    Failed { effect: ControlEffect },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlTransition {
    pub from: ControlState,
    pub to: ControlState,
    pub reason: ControlReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlMachine {
    state: ControlState,
    history: Vec<ControlTransition>,
}

impl Default for ControlMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlMachine {
    pub fn new() -> Self {
        Self {
            state: ControlState::Requested,
            history: Vec::new(),
        }
    }

    pub fn state(&self) -> ControlState {
        self.state
    }

    pub fn history(&self) -> &[ControlTransition] {
        &self.history
    }

    pub fn authorize(
        &mut self,
        controller: &dyn ControlController,
        request: ControlRequest<'_>,
    ) -> anyhow::Result<ControlTransition> {
        if self.state != ControlState::Requested {
            anyhow::bail!(
                "control request was already authorized from {:?}",
                self.state
            );
        }
        let transition = controller.authorize(request);
        if transition.from != ControlState::Requested
            || !matches!(transition.to, ControlState::Allowed | ControlState::Denied)
        {
            anyhow::bail!(
                "controller returned invalid authorization transition {:?} -> {:?}",
                transition.from,
                transition.to
            );
        }
        self.commit(transition)
    }

    pub fn applied(&mut self) -> anyhow::Result<ControlTransition> {
        let transition = self
            .history
            .last()
            .ok_or_else(|| anyhow::anyhow!("control request has not been authorized"))?
            .applied()?;
        self.commit(transition)
    }

    pub fn failed(&mut self) -> anyhow::Result<ControlTransition> {
        let transition = self
            .history
            .last()
            .ok_or_else(|| anyhow::anyhow!("control request has not been authorized"))?
            .failed()?;
        self.commit(transition)
    }

    fn commit(&mut self, transition: ControlTransition) -> anyhow::Result<ControlTransition> {
        if transition.from != self.state {
            anyhow::bail!(
                "control state mismatch: current {:?}, transition starts at {:?}",
                self.state,
                transition.from
            );
        }
        self.state = transition.to;
        self.history.push(transition.clone());
        Ok(transition)
    }
}

impl ControlTransition {
    pub fn allowed(reason: ControlReason) -> Self {
        Self {
            from: ControlState::Requested,
            to: ControlState::Allowed,
            reason,
        }
    }

    pub fn denied(reason: ControlReason) -> Self {
        Self {
            from: ControlState::Requested,
            to: ControlState::Denied,
            reason,
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(
            self.to,
            ControlState::Allowed
                | ControlState::Applied {
                    effect: ControlEffect::Allow
                }
        )
    }

    pub fn applied(&self) -> anyhow::Result<Self> {
        self.follow(true)
    }

    pub fn failed(&self) -> anyhow::Result<Self> {
        self.follow(false)
    }

    fn follow(&self, applied: bool) -> anyhow::Result<Self> {
        let effect = match self.to {
            ControlState::Allowed => ControlEffect::Allow,
            ControlState::Denied => ControlEffect::Deny,
            state => anyhow::bail!(
                "control transition {state:?} cannot be applied; authorization must happen first"
            ),
        };
        let to = if applied {
            ControlState::Applied { effect }
        } else {
            ControlState::Failed { effect }
        };
        Ok(Self {
            from: self.to,
            to,
            reason: self.reason,
        })
    }
}

pub enum ControlRequest<'a> {
    Network {
        policy: &'a NetworkGuard,
        request: &'a NetworkAccessRequest,
    },
    Model {
        policy: &'a ModelAccessPolicy,
        request: &'a ModelCallRequest,
    },
}

pub trait ControlController: Send + Sync {
    fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition;
}

#[derive(Debug, Default)]
pub struct PolicyControlController;

impl ControlController for PolicyControlController {
    fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
        match request {
            ControlRequest::Network { policy, request } => authorize_network(policy, request),
            ControlRequest::Model { policy, request } => authorize_model(policy, request),
        }
    }
}

fn authorize_network(policy: &NetworkGuard, request: &NetworkAccessRequest) -> ControlTransition {
    if policy.denied(request) {
        return ControlTransition::denied(ControlReason::ExplicitlyDenied);
    }
    if policy.is_trusted(&request.host) {
        return ControlTransition::allowed(ControlReason::TrustedLocal);
    }
    match policy.capability() {
        NetworkCapability::Ambient => ControlTransition::allowed(ControlReason::AmbientNetwork),
        NetworkCapability::Deny => ControlTransition::denied(ControlReason::NetworkDenied),
        NetworkCapability::AllowList { .. } => match policy.evaluate(request) {
            Ok(()) => ControlTransition::allowed(ControlReason::NetworkAllowList),
            Err(NetworkMatchFailure::Empty) => {
                ControlTransition::denied(ControlReason::NetworkAllowListEmpty)
            }
            Err(NetworkMatchFailure::Host) => {
                ControlTransition::denied(ControlReason::HostNotAllowed)
            }
            Err(NetworkMatchFailure::Port) => {
                ControlTransition::denied(ControlReason::PortNotAllowed)
            }
            Err(NetworkMatchFailure::Transport) => {
                ControlTransition::denied(ControlReason::TransportNotAllowed)
            }
            Err(NetworkMatchFailure::ResolvedAddress) => {
                ControlTransition::denied(ControlReason::ResolvedAddressNotAllowed)
            }
        },
        NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Allow,
            ..
        } => ControlTransition::allowed(ControlReason::AmbientNetwork),
        NetworkCapability::Policy {
            default_action: NetworkDefaultAction::Deny,
            ..
        } => match policy.evaluate(request) {
            Ok(()) => ControlTransition::allowed(ControlReason::NetworkAllowList),
            Err(NetworkMatchFailure::Empty) => {
                ControlTransition::denied(ControlReason::NetworkAllowListEmpty)
            }
            Err(NetworkMatchFailure::Host) => {
                ControlTransition::denied(ControlReason::HostNotAllowed)
            }
            Err(NetworkMatchFailure::Port) => {
                ControlTransition::denied(ControlReason::PortNotAllowed)
            }
            Err(NetworkMatchFailure::Transport) => {
                ControlTransition::denied(ControlReason::TransportNotAllowed)
            }
            Err(NetworkMatchFailure::ResolvedAddress) => {
                ControlTransition::denied(ControlReason::ResolvedAddressNotAllowed)
            }
        },
    }
}

fn authorize_model(policy: &ModelAccessPolicy, request: &ModelCallRequest) -> ControlTransition {
    let model_allowed = policy.allowed_models.iter().any(|pattern| {
        model_matches(pattern, &request.client_model)
            || model_matches(pattern, &request.upstream_model)
    });
    if !model_allowed {
        return ControlTransition::denied(ControlReason::ModelNotAllowed);
    }
    if !policy.allowed_providers.is_empty()
        && !policy
            .allowed_providers
            .iter()
            .any(|provider| provider.eq_ignore_ascii_case(&request.provider))
    {
        return ControlTransition::denied(ControlReason::ProviderNotAllowed);
    }
    ControlTransition::allowed(ControlReason::ModelAllowed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkHostRule {
    Exact(String),
    WildcardSuffix(String),
    Ip(IpAddr),
    Cidr(IpNet),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRule {
    pub host: NetworkHostRule,
    pub ports: Vec<u16>,
    pub transports: Vec<NetworkTransport>,
    pub allow_private_ips: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkMatchFailure {
    Empty,
    Host,
    Port,
    Transport,
    ResolvedAddress,
}

#[derive(Debug, Clone)]
pub struct NetworkGuard {
    capability: NetworkCapability,
    rules: Vec<NetworkRule>,
    deny_rules: Vec<NetworkRule>,
    trusted_hosts: Vec<String>,
}

impl NetworkGuard {
    pub fn compile(
        capability: NetworkCapability,
        trusted_hosts: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        let rules = match &capability {
            NetworkCapability::AllowList { hosts, rules } => {
                let mut compiled = hosts
                    .iter()
                    .map(|host| parse_network_rule(host))
                    .collect::<anyhow::Result<Vec<_>>>()?;
                compiled.extend(
                    rules
                        .iter()
                        .map(compile_network_rule)
                        .collect::<anyhow::Result<Vec<_>>>()?,
                );
                compiled
            }
            NetworkCapability::Policy { allow, .. } => allow
                .iter()
                .map(compile_network_rule)
                .collect::<anyhow::Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        let deny_rules = match &capability {
            NetworkCapability::Policy { deny, .. } => deny
                .iter()
                .map(compile_network_rule)
                .collect::<anyhow::Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Ok(Self {
            capability,
            rules,
            deny_rules,
            trusted_hosts: trusted_hosts
                .into_iter()
                .map(|host| normalize_host(&host))
                .collect(),
        })
    }

    pub fn capability(&self) -> &NetworkCapability {
        &self.capability
    }

    pub fn rules(&self) -> &[NetworkRule] {
        &self.rules
    }

    fn evaluate(&self, request: &NetworkAccessRequest) -> Result<(), NetworkMatchFailure> {
        evaluate_rules(&self.rules, request, true)
    }

    fn denied(&self, request: &NetworkAccessRequest) -> bool {
        evaluate_rules(&self.deny_rules, request, false).is_ok()
    }

    fn is_trusted(&self, host: &str) -> bool {
        let host = normalize_host(host);
        self.trusted_hosts.iter().any(|trusted| trusted == &host)
    }
}

fn evaluate_rules(
    rules: &[NetworkRule],
    request: &NetworkAccessRequest,
    enforce_resolved_address_safety: bool,
) -> Result<(), NetworkMatchFailure> {
    if rules.is_empty() {
        return Err(NetworkMatchFailure::Empty);
    }
    let host_rules: Vec<&NetworkRule> = rules
        .iter()
        .filter(|rule| rule.matches_host(&request.host, request.resolved_ip))
        .collect();
    if host_rules.is_empty() {
        return Err(if request.resolved_ip.is_some() {
            NetworkMatchFailure::ResolvedAddress
        } else {
            NetworkMatchFailure::Host
        });
    }
    let port_rules: Vec<&NetworkRule> = host_rules
        .into_iter()
        .filter(|rule| rule.matches_port(request.port))
        .collect();
    if port_rules.is_empty() {
        return Err(NetworkMatchFailure::Port);
    }
    let transport_rules: Vec<&NetworkRule> = port_rules
        .into_iter()
        .filter(|rule| rule.matches_transport(request.transport))
        .collect();
    if transport_rules.is_empty() {
        return Err(NetworkMatchFailure::Transport);
    }
    if enforce_resolved_address_safety
        && request.resolved_ip.is_some()
        && !transport_rules
            .iter()
            .any(|rule| rule.allows_resolved_address(request.resolved_ip))
    {
        return Err(NetworkMatchFailure::ResolvedAddress);
    }
    Ok(())
}

impl NetworkRule {
    fn matches_host(&self, host: &str, resolved_ip: Option<IpAddr>) -> bool {
        let host = normalize_host(host);
        let host_ip = IpAddr::from_str(&host).ok();
        match &self.host {
            NetworkHostRule::Exact(allowed) => host == *allowed,
            NetworkHostRule::WildcardSuffix(suffix) => {
                host.ends_with(suffix)
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            }
            NetworkHostRule::Ip(allowed) => {
                host_ip == Some(*allowed) || resolved_ip == Some(*allowed)
            }
            NetworkHostRule::Cidr(network) => host_ip
                .or(resolved_ip)
                .is_some_and(|ip| network.contains(&ip)),
        }
    }

    fn matches_port(&self, port: Option<u16>) -> bool {
        self.ports.is_empty() || port.is_some_and(|port| self.ports.contains(&port))
    }

    fn matches_transport(&self, transport: NetworkTransport) -> bool {
        self.transports.is_empty() || self.transports.contains(&transport)
    }

    fn allows_resolved_address(&self, resolved_ip: Option<IpAddr>) -> bool {
        let Some(ip) = resolved_ip else {
            return true;
        };
        match &self.host {
            NetworkHostRule::Ip(allowed) => ip == *allowed,
            NetworkHostRule::Cidr(network) => network.contains(&ip),
            NetworkHostRule::Exact(_) | NetworkHostRule::WildcardSuffix(_) => {
                is_public_egress_ip(ip) || (self.allow_private_ips && is_opt_in_private_ip(ip))
            }
        }
    }
}

fn is_opt_in_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_opt_in_private_ip(IpAddr::V4(mapped));
            }
            ip.is_loopback() || ip.segments()[0] & 0xfe00 == 0xfc00
        }
    }
}

pub fn normalize_host(host: &str) -> String {
    host.trim()
        .trim_matches(|character| character == '[' || character == ']')
        .to_ascii_lowercase()
        .trim_end_matches('.')
        .to_string()
}

pub fn parse_network_rule(raw: &str) -> anyhow::Result<NetworkRule> {
    compile_network_rule(&NetworkAccessRule {
        host: raw.to_string(),
        ports: Vec::new(),
        transports: Vec::new(),
        allow_private_ips: false,
    })
}

fn compile_network_rule(rule: &NetworkAccessRule) -> anyhow::Result<NetworkRule> {
    if rule.ports.contains(&0) {
        anyhow::bail!("network rule ports must not contain zero");
    }
    let mut ports = rule.ports.clone();
    ports.sort_unstable();
    ports.dedup();
    let mut transports = rule.transports.clone();
    transports.dedup();

    let host = parse_network_host_rule(&rule.host)?;
    Ok(NetworkRule {
        host,
        ports,
        transports,
        allow_private_ips: rule.allow_private_ips,
    })
}

fn parse_network_host_rule(raw: &str) -> anyhow::Result<NetworkHostRule> {
    let entry = raw.trim();
    if entry.is_empty() {
        anyhow::bail!("network allowlist entry must not be empty");
    }
    if entry.contains("://") || entry.contains(']') || entry.contains('[') {
        anyhow::bail!(
            "network allowlist entry `{entry}` must be a hostname, `*.suffix`, IP, or CIDR"
        );
    }
    if entry.contains('/') && IpNet::from_str(entry).is_err() {
        anyhow::bail!(
            "network allowlist entry `{entry}` must be a hostname, `*.suffix`, IP, or CIDR"
        );
    }
    if let Some((host, port)) = entry.rsplit_once(':') {
        if !host.is_empty()
            && port.chars().all(|character| character.is_ascii_digit())
            && !entry.contains('/')
            && host.parse::<IpAddr>().is_err()
            && !host.contains(':')
        {
            anyhow::bail!("network allowlist entry `{entry}` must not include a port");
        }
    }
    if let Some(suffix) = entry.strip_prefix("*.") {
        let suffix = normalize_host(suffix);
        if suffix.is_empty() || suffix.contains('*') || suffix.parse::<IpAddr>().is_ok() {
            anyhow::bail!("invalid wildcard network allowlist entry `{entry}`");
        }
        return Ok(NetworkHostRule::WildcardSuffix(suffix));
    }
    if entry.contains('*') {
        anyhow::bail!("only leading `*.suffix` host wildcards are supported");
    }
    if let Ok(network) = IpNet::from_str(entry) {
        if entry.contains('/') {
            return Ok(NetworkHostRule::Cidr(network));
        }
        return Ok(NetworkHostRule::Ip(network.addr()));
    }
    if let Ok(ip) = IpAddr::from_str(entry) {
        return Ok(NetworkHostRule::Ip(ip));
    }
    let host = normalize_host(entry);
    if host.is_empty() || host.contains(':') {
        anyhow::bail!("invalid network allowlist hostname `{entry}`");
    }
    Ok(NetworkHostRule::Exact(host))
}

pub fn host_matches(host: &str, rules: &[NetworkRule]) -> bool {
    rules.iter().any(|rule| rule.matches_host(host, None))
}

/// Whether an address is suitable as the destination of a hostname rule
/// without an explicit private-address opt-in.
pub fn is_public_egress_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            octets[0] != 0
                && !ip.is_loopback()
                && !ip.is_private()
                && !ip.is_link_local()
                && !ip.is_multicast()
                && !ip.is_broadcast()
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                && !(octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                && !(octets[0] == 198 && (18..=19).contains(&octets[1]))
                && !(octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                && !(octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                && octets[0] < 240
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public_egress_ip(IpAddr::V4(mapped));
            }
            let segments = ip.segments();
            // Only globally-routed unicast space is eligible. This also
            // rejects IPv4-compatible, site-local, link-local, ULA, and other
            // special-purpose prefixes before considering narrower carveouts.
            (segments[0] & 0xe000 == 0x2000)
                && !(segments[0] == 0x2001 && segments[1] <= 0x01ff)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
        }
    }
}

fn model_matches(pattern: &str, model: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return !prefix.is_empty() && model.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return !suffix.is_empty() && model.ends_with(suffix);
    }
    pattern == model
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NetworkTransport;

    fn network_request(host: &str) -> NetworkAccessRequest {
        NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            host: host.into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        }
    }

    #[test]
    fn authorization_is_a_control_transition() {
        let controller = PolicyControlController;
        let guard = NetworkGuard::compile(NetworkCapability::Deny, Vec::new()).unwrap();
        let transition = controller.authorize(ControlRequest::Network {
            policy: &guard,
            request: &network_request("example.com"),
        });
        assert_eq!(transition.from, ControlState::Requested);
        assert_eq!(transition.to, ControlState::Denied);
        assert!(!transition.is_allowed());
        assert_eq!(
            transition.applied().unwrap().to,
            ControlState::Applied {
                effect: ControlEffect::Deny
            }
        );
    }

    #[test]
    fn allowed_operation_can_transition_to_applied_or_failed() {
        let transition = ControlTransition::allowed(ControlReason::TrustedLocal);
        assert_eq!(
            transition.applied().unwrap().to,
            ControlState::Applied {
                effect: ControlEffect::Allow
            }
        );
        assert_eq!(
            transition.failed().unwrap().to,
            ControlState::Failed {
                effect: ControlEffect::Allow
            }
        );
    }

    #[test]
    fn machine_owns_state_and_transition_history() {
        let controller = PolicyControlController;
        let guard = NetworkGuard::compile(NetworkCapability::Ambient, Vec::new()).unwrap();
        let request = network_request("example.com");
        let mut machine = ControlMachine::new();
        let decision = machine
            .authorize(
                &controller,
                ControlRequest::Network {
                    policy: &guard,
                    request: &request,
                },
            )
            .unwrap();
        assert!(decision.is_allowed());
        assert_eq!(machine.state(), ControlState::Allowed);
        machine.applied().unwrap();
        assert_eq!(
            machine.state(),
            ControlState::Applied {
                effect: ControlEffect::Allow
            }
        );
        assert_eq!(machine.history().len(), 2);
        let encoded = serde_json::to_string(&machine).unwrap();
        let restored: ControlMachine = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.state(), machine.state());
        assert_eq!(restored.history(), machine.history());
        assert!(machine
            .authorize(
                &controller,
                ControlRequest::Network {
                    policy: &guard,
                    request: &request,
                },
            )
            .is_err());
    }

    #[test]
    fn policy_controller_handles_network_and_model_resources() {
        let controller = PolicyControlController;
        let guard = NetworkGuard::compile(
            NetworkCapability::AllowList {
                hosts: vec!["*.example.com".into(), "10.0.0.0/8".into()],
                rules: Vec::new(),
            },
            Vec::new(),
        )
        .unwrap();
        assert!(controller
            .authorize(ControlRequest::Network {
                policy: &guard,
                request: &network_request("api.example.com"),
            })
            .is_allowed());

        let policy = ModelAccessPolicy {
            allowed_models: vec!["claude-*".into()],
            allowed_providers: vec!["anthropic".into()],
        };
        let request = ModelCallRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            call_id: "call-1".into(),
            client_model: "claude-sonnet".into(),
            upstream_model: "claude-sonnet".into(),
            provider: "anthropic".into(),
            protocol: "messages".into(),
            upstream_host: "api.anthropic.com".into(),
        };
        assert!(controller
            .authorize(ControlRequest::Model {
                policy: &policy,
                request: &request,
            })
            .is_allowed());
    }

    #[test]
    fn loopback_is_only_trusted_when_explicitly_configured() {
        let controller = PolicyControlController;
        let denied = NetworkGuard::compile(NetworkCapability::Deny, Vec::new()).unwrap();
        assert!(!controller
            .authorize(ControlRequest::Network {
                policy: &denied,
                request: &network_request("127.0.0.1"),
            })
            .is_allowed());

        let trusted = NetworkGuard::compile(NetworkCapability::Deny, ["127.0.0.1".into()]).unwrap();
        let transition = controller.authorize(ControlRequest::Network {
            policy: &trusted,
            request: &network_request("127.0.0.1"),
        });
        assert!(transition.is_allowed());
        assert_eq!(transition.reason, ControlReason::TrustedLocal);
    }

    #[test]
    fn structured_rules_constrain_port_transport_and_resolved_address() {
        let controller = PolicyControlController;
        let guard = NetworkGuard::compile(
            NetworkCapability::AllowList {
                hosts: Vec::new(),
                rules: vec![NetworkAccessRule {
                    host: "api.example.com".into(),
                    ports: vec![443],
                    transports: vec![NetworkTransport::Https],
                    allow_private_ips: false,
                }],
            },
            Vec::new(),
        )
        .unwrap();
        let decide = |port, transport, resolved_ip| {
            controller.authorize(ControlRequest::Network {
                policy: &guard,
                request: &NetworkAccessRequest {
                    run_id: None,
                    attempt_id: None,
                    storyline_id: None,
                    host: "api.example.com".into(),
                    port: Some(port),
                    transport,
                    resolved_ip,
                },
            })
        };

        assert!(decide(
            443,
            NetworkTransport::Https,
            Some("93.184.216.34".parse().unwrap())
        )
        .is_allowed());
        assert_eq!(
            decide(
                8443,
                NetworkTransport::Https,
                Some("93.184.216.34".parse().unwrap())
            )
            .reason,
            ControlReason::PortNotAllowed
        );
        assert_eq!(
            decide(
                443,
                NetworkTransport::TcpTunnel,
                Some("93.184.216.34".parse().unwrap())
            )
            .reason,
            ControlReason::TransportNotAllowed
        );
        assert_eq!(
            decide(
                443,
                NetworkTransport::Https,
                Some("127.0.0.1".parse().unwrap())
            )
            .reason,
            ControlReason::ResolvedAddressNotAllowed
        );
    }

    #[test]
    fn explicit_ip_and_private_opt_in_allow_private_destinations() {
        let controller = PolicyControlController;
        for rule in [
            NetworkAccessRule {
                host: "127.0.0.1".into(),
                ports: vec![8080],
                transports: vec![NetworkTransport::Http],
                allow_private_ips: false,
            },
            NetworkAccessRule {
                host: "local.example".into(),
                ports: vec![8080],
                transports: vec![NetworkTransport::Http],
                allow_private_ips: true,
            },
        ] {
            let host = rule.host.clone();
            let guard = NetworkGuard::compile(
                NetworkCapability::AllowList {
                    hosts: Vec::new(),
                    rules: vec![rule],
                },
                Vec::new(),
            )
            .unwrap();
            let transition = controller.authorize(ControlRequest::Network {
                policy: &guard,
                request: &NetworkAccessRequest {
                    run_id: None,
                    attempt_id: None,
                    storyline_id: None,
                    host,
                    port: Some(8080),
                    transport: NetworkTransport::Http,
                    resolved_ip: Some("127.0.0.1".parse().unwrap()),
                },
            });
            assert!(transition.is_allowed());
        }
    }

    #[test]
    fn address_classifier_rejects_local_and_metadata_ranges() {
        for address in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "::192.0.2.1",
            "2001::1",
            "2001:db8::1",
            "2002::1",
            "3fff::1",
        ] {
            assert!(!is_public_egress_ip(address.parse().unwrap()), "{address}");
        }
        assert!(is_public_egress_ip("1.1.1.1".parse().unwrap()));
        assert!(is_public_egress_ip("8.8.8.8".parse().unwrap()));
        assert!(is_public_egress_ip("2001:4860:4860::8888".parse().unwrap()));
        assert!(is_public_egress_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn private_opt_in_does_not_include_link_local_or_special_addresses() {
        assert!(is_opt_in_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_opt_in_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_opt_in_private_ip("fc00::1".parse().unwrap()));
        assert!(!is_opt_in_private_ip("169.254.169.254".parse().unwrap()));
        assert!(!is_opt_in_private_ip("224.0.0.1".parse().unwrap()));
        assert!(!is_opt_in_private_ip("fe80::1".parse().unwrap()));
    }
}
