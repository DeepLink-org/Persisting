//! pVisor policy decisions for resources reached through compatibility proxies.

use ipnet::IpNet;
use persisting_proto::{
    AccessDecision, AccessReason, ModelAccessPolicy, ModelCallRequest, NetworkAccessRequest,
    NetworkCapability,
};
use std::net::IpAddr;
use std::str::FromStr;

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

pub trait AccessController: Send + Sync {
    fn authorize_network(
        &self,
        policy: &NetworkGuard,
        request: &NetworkAccessRequest,
    ) -> AccessDecision;

    fn authorize_model(
        &self,
        policy: &ModelAccessPolicy,
        request: &ModelCallRequest,
    ) -> AccessDecision;
}

#[derive(Debug, Default)]
pub struct PolicyAccessController;

impl AccessController for PolicyAccessController {
    fn authorize_network(
        &self,
        policy: &NetworkGuard,
        request: &NetworkAccessRequest,
    ) -> AccessDecision {
        if policy.is_trusted(&request.host) {
            return AccessDecision::allow(AccessReason::TrustedLocal);
        }
        match policy.capability() {
            NetworkCapability::Ambient => AccessDecision::allow(AccessReason::AmbientNetwork),
            NetworkCapability::Deny => AccessDecision::deny(AccessReason::NetworkDenied),
            NetworkCapability::AllowList { .. } if policy.rules().is_empty() => {
                AccessDecision::deny(AccessReason::NetworkAllowListEmpty)
            }
            NetworkCapability::AllowList { .. } if host_matches(&request.host, policy.rules()) => {
                AccessDecision::allow(AccessReason::NetworkAllowList)
            }
            NetworkCapability::AllowList { .. } => {
                AccessDecision::deny(AccessReason::HostNotAllowed)
            }
        }
    }

    fn authorize_model(
        &self,
        policy: &ModelAccessPolicy,
        request: &ModelCallRequest,
    ) -> AccessDecision {
        let model_allowed = policy.allowed_models.iter().any(|pattern| {
            model_matches(pattern, &request.client_model)
                || model_matches(pattern, &request.upstream_model)
        });
        if !model_allowed {
            return AccessDecision::deny(AccessReason::ModelNotAllowed);
        }
        if !policy.allowed_providers.is_empty()
            && !policy
                .allowed_providers
                .iter()
                .any(|provider| provider.eq_ignore_ascii_case(&request.provider))
        {
            return AccessDecision::deny(AccessReason::ProviderNotAllowed);
        }
        AccessDecision::allow(AccessReason::ModelAllowed)
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

    fn request(host: &str) -> NetworkAccessRequest {
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
    fn network_policy_handles_trusted_deny_and_allowlist() {
        let controller = PolicyAccessController;
        let deny = NetworkGuard::compile(NetworkCapability::Deny, ["proxy.local".into()]).unwrap();
        assert!(controller
            .authorize_network(&deny, &request("127.0.0.1"))
            .is_allowed());
        assert!(!controller
            .authorize_network(&deny, &request("example.com"))
            .is_allowed());

        let allow = NetworkGuard::compile(
            NetworkCapability::AllowList {
                hosts: vec!["*.example.com".into(), "10.0.0.0/8".into()],
            },
            Vec::new(),
        )
        .unwrap();
        assert!(controller
            .authorize_network(&allow, &request("api.example.com"))
            .is_allowed());
        assert!(controller
            .authorize_network(&allow, &request("10.1.2.3"))
            .is_allowed());
        assert!(!controller
            .authorize_network(&allow, &request("example.com"))
            .is_allowed());
    }

    #[test]
    fn model_policy_checks_model_and_provider() {
        let controller = PolicyAccessController;
        let policy = ModelAccessPolicy {
            allowed_models: vec!["claude-*".into()],
            allowed_providers: vec!["anthropic".into()],
        };
        let mut request = ModelCallRequest {
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
        assert!(controller.authorize_model(&policy, &request).is_allowed());
        request.provider = "custom".into();
        assert!(!controller.authorize_model(&policy, &request).is_allowed());
    }
}
