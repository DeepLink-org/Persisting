//! Unified control state and transition protocol.
//!
//! Drivers such as OverlayNet and Capture submit typed resources to a
//! [`ControlController`]. Authorization is represented as a state transition;
//! the driver then records whether the authorized operation was applied or
//! failed. The crate decides policy but does not perform network, model, or
//! filesystem operations itself.

use ipnet::IpNet;
pub use persisting_proto::{AccessEffect as ControlEffect, AccessReason as ControlReason};
use persisting_proto::{
    ModelAccessPolicy, ModelCallRequest, NetworkAccessRequest, NetworkCapability,
};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::str::FromStr;

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
    if policy.is_trusted(&request.host) {
        return ControlTransition::allowed(ControlReason::TrustedLocal);
    }
    match policy.capability() {
        NetworkCapability::Ambient => ControlTransition::allowed(ControlReason::AmbientNetwork),
        NetworkCapability::Deny => ControlTransition::denied(ControlReason::NetworkDenied),
        NetworkCapability::AllowList { .. } if policy.rules().is_empty() => {
            ControlTransition::denied(ControlReason::NetworkAllowListEmpty)
        }
        NetworkCapability::AllowList { .. } if host_matches(&request.host, policy.rules()) => {
            ControlTransition::allowed(ControlReason::NetworkAllowList)
        }
        NetworkCapability::AllowList { .. } => {
            ControlTransition::denied(ControlReason::HostNotAllowed)
        }
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
pub enum NetworkRule {
    Exact(String),
    WildcardSuffix(String),
    Ip(IpAddr),
    Cidr(IpNet),
}

#[derive(Debug, Clone)]
pub struct NetworkGuard {
    capability: NetworkCapability,
    rules: Vec<NetworkRule>,
    trusted_hosts: Vec<String>,
}

impl NetworkGuard {
    pub fn compile(
        capability: NetworkCapability,
        trusted_hosts: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        let rules = match &capability {
            NetworkCapability::AllowList { hosts } => hosts
                .iter()
                .map(|host| parse_network_rule(host))
                .collect::<anyhow::Result<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Ok(Self {
            capability,
            rules,
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

    fn is_trusted(&self, host: &str) -> bool {
        let host = normalize_host(host);
        if matches!(
            host.as_str(),
            "localhost" | "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1"
        ) {
            return true;
        }
        if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()) {
            return true;
        }
        self.trusted_hosts.iter().any(|trusted| trusted == &host)
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
        return Ok(NetworkRule::WildcardSuffix(suffix));
    }
    if entry.contains('*') {
        anyhow::bail!("only leading `*.suffix` host wildcards are supported");
    }
    if let Ok(network) = IpNet::from_str(entry) {
        if entry.contains('/') {
            return Ok(NetworkRule::Cidr(network));
        }
        return Ok(NetworkRule::Ip(network.addr()));
    }
    if let Ok(ip) = IpAddr::from_str(entry) {
        return Ok(NetworkRule::Ip(ip));
    }
    let host = normalize_host(entry);
    if host.is_empty() || host.contains(':') {
        anyhow::bail!("invalid network allowlist hostname `{entry}`");
    }
    Ok(NetworkRule::Exact(host))
}

pub fn host_matches(host: &str, rules: &[NetworkRule]) -> bool {
    let host = normalize_host(host);
    let host_ip = IpAddr::from_str(&host).ok();
    rules.iter().any(|rule| match rule {
        NetworkRule::Exact(allowed) => host == *allowed,
        NetworkRule::WildcardSuffix(suffix) => {
            host.ends_with(suffix)
                && host.len() > suffix.len()
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
        }
        NetworkRule::Ip(allowed) => host_ip == Some(*allowed),
        NetworkRule::Cidr(network) => host_ip.is_some_and(|ip| network.contains(&ip)),
    })
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
    use persisting_proto::NetworkTransport;

    fn network_request(host: &str) -> NetworkAccessRequest {
        NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            host: host.into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
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
}
