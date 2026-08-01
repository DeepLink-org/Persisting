//! Egress policy for the explicit HTTP proxy backend.

use axum::http::StatusCode;
pub use persisting_control::{
    host_matches, normalize_host, parse_network_rule as parse_allowed_entry,
    NetworkRule as AllowedEntry,
};
use persisting_control::{
    ControlController, ControlMachine, ControlReason, ControlRequest, NetworkGuard,
};
use persisting_proto::{NetworkAccessRequest, NetworkCapability};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    #[default]
    Public,
    NoNetwork,
    Allowlist,
}

/// Minimal configuration view needed to compile an overlaynet policy.
pub trait PolicyConfig {
    fn listen(&self) -> &str;
    fn network(&self) -> &NetworkConfig;
    fn trusted_upstream_hosts(&self) -> Vec<String>;
}

#[derive(Debug, Clone)]
pub struct NetworkPolicy {
    mode: NetworkMode,
    guard: NetworkGuard,
}

impl NetworkPolicy {
    pub fn compile(
        listen: &str,
        network: &NetworkConfig,
        trusted_upstream_hosts: impl IntoIterator<Item = String>,
    ) -> anyhow::Result<Self> {
        let listen_host = host_from_listen(listen);
        let capability = network_capability(network, trusted_upstream_hosts);
        let guard = NetworkGuard::compile(capability, [listen_host.clone()])?;
        Ok(Self {
            mode: network.mode,
            guard,
        })
    }

    pub fn from_config(config: &impl PolicyConfig) -> anyhow::Result<Self> {
        Self::compile(
            config.listen(),
            config.network(),
            config.trusted_upstream_hosts(),
        )
    }

    pub fn mode_str(&self) -> &'static str {
        match self.mode {
            NetworkMode::Public => "public",
            NetworkMode::NoNetwork => "no-network",
            NetworkMode::Allowlist => "allowlist",
        }
    }
}

pub fn network_capability(
    network: &NetworkConfig,
    trusted_upstream_hosts: impl IntoIterator<Item = String>,
) -> NetworkCapability {
    match network.mode {
        NetworkMode::Public => NetworkCapability::Ambient,
        NetworkMode::NoNetwork => NetworkCapability::Deny,
        NetworkMode::Allowlist => {
            let mut hosts = network.allowed_hosts.clone();
            for host in trusted_upstream_hosts {
                let normalized = normalize_host(&host);
                if !normalized.is_empty()
                    && !hosts
                        .iter()
                        .any(|candidate| normalize_host(candidate) == normalized)
                {
                    hosts.push(normalized);
                }
            }
            NetworkCapability::AllowList { hosts }
        }
    }
}

pub fn network_capability_from_config(config: &impl PolicyConfig) -> NetworkCapability {
    network_capability(config.network(), config.trusted_upstream_hosts())
}

pub fn validate_network_config(network: &NetworkConfig) -> anyhow::Result<()> {
    for entry in &network.allowed_hosts {
        parse_allowed_entry(entry)?;
    }
    Ok(())
}

pub fn host_from_listen(listen: &str) -> String {
    let value = listen.trim();
    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value);
    host_from_authority(without_scheme.trim_end_matches('/'))
}

pub fn host_from_authority(authority: &str) -> String {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return normalize_host(&rest[..end]);
        }
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() && port.chars().all(|character| character.is_ascii_digit()) {
            return normalize_host(host);
        }
    }
    normalize_host(authority)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    NoNetwork,
    AllowlistEmpty,
    NotInAllowlist,
}

impl DenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoNetwork => "no-network",
            Self::AllowlistEmpty => "allowlist-empty",
            Self::NotInAllowlist => "not-in-allowlist",
        }
    }
}

pub fn authorize_egress(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    request: &NetworkAccessRequest,
) -> Result<(), DenyReason> {
    let mut control = ControlMachine::new();
    let transition = control
        .authorize(
            controller,
            ControlRequest::Network {
                policy: &policy.guard,
                request,
            },
        )
        .expect("policy controller must return a valid authorization transition");
    let allowed = transition.is_allowed();
    let reason = transition.reason;
    let _applied = control
        .applied()
        .expect("an authorization transition can be applied");
    if allowed {
        return Ok(());
    }
    Err(match reason {
        ControlReason::NetworkDenied => DenyReason::NoNetwork,
        ControlReason::NetworkAllowListEmpty => DenyReason::AllowlistEmpty,
        _ => DenyReason::NotInAllowlist,
    })
}

pub fn forbidden_response(host: &str, reason: &DenyReason) -> (StatusCode, String) {
    (
        StatusCode::FORBIDDEN,
        format!(
            "persisting-overlaynet: egress to `{host}` denied ({})",
            reason.as_str()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_control::PolicyControlController;
    use persisting_proto::NetworkTransport;

    #[test]
    fn allowlist_includes_trusted_upstream_once() {
        let config = NetworkConfig {
            mode: NetworkMode::Allowlist,
            allowed_hosts: vec!["*.example.com".into()],
        };
        let capability = network_capability(&config, ["api.example.net".into()]);
        assert_eq!(
            capability,
            NetworkCapability::AllowList {
                hosts: vec!["*.example.com".into(), "api.example.net".into()]
            }
        );
    }

    #[test]
    fn deny_blocks_public_host_but_keeps_loopback() {
        let policy = NetworkPolicy::compile(
            "127.0.0.1:19081",
            &NetworkConfig {
                mode: NetworkMode::NoNetwork,
                allowed_hosts: Vec::new(),
            },
            Vec::new(),
        )
        .unwrap();
        let request = |host: &str| NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            host: host.to_string(),
            port: None,
            transport: NetworkTransport::TcpTunnel,
        };
        assert!(
            authorize_egress(&PolicyControlController, &policy, &request("example.com")).is_err()
        );
        assert!(authorize_egress(&PolicyControlController, &policy, &request("127.0.0.1")).is_ok());
    }
}
