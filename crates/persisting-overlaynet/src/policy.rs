//! Egress policy for the explicit HTTP proxy backend.

use axum::http::StatusCode;
use persisting_agentctl::{
    ControlController, ControlMachine, ControlReason, ControlRequest, NetworkGuard,
    PolicyControlController,
};
use persisting_agentctl::{NetworkAccessRequest, NetworkCapability, NetworkDefaultAction};
pub use persisting_agentctl::{NetworkAccessRule, NetworkBandwidthLimit};
pub use persisting_agentctl::{
    NetworkRule as AllowedEntry, host_matches, normalize_host,
    parse_network_rule as parse_allowed_entry,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Structured grants. Prefer these when port, transport, or private-address
    /// behavior must be constrained explicitly.
    #[serde(default)]
    pub rules: Vec<NetworkAccessRule>,
    /// Explicit deny rules. These take precedence over every allow rule.
    #[serde(default)]
    pub deny_rules: Vec<NetworkAccessRule>,
    /// Aggregate bandwidth constraints. Every matching constraint applies.
    #[serde(default)]
    pub limits: Vec<NetworkBandwidthLimit>,
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
    fn network(&self) -> &NetworkConfig;
}

#[derive(Debug, Clone)]
pub struct NetworkPolicy {
    mode: NetworkMode,
    guard: NetworkGuard,
    limits: Vec<CompiledBandwidthLimit>,
}

#[derive(Debug, Clone)]
struct CompiledBandwidthLimit {
    matcher: Option<AllowedEntry>,
    config: NetworkBandwidthLimit,
}

impl NetworkPolicy {
    pub fn compile(network: &NetworkConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(
            network.mode == NetworkMode::Allowlist
                || (network.allowed_hosts.is_empty() && network.rules.is_empty()),
            "network allow entries require mode = \"allowlist\""
        );
        let capability = network_capability(network);
        // Gateway-owned upstream routes and the listener itself are not Agent
        // egress grants. Only explicit entries from `[network]` reach this guard.
        let guard = NetworkGuard::compile(capability, Vec::new())?;
        let limits = network
            .limits
            .iter()
            .map(|config| {
                anyhow::ensure!(
                    config.bytes_per_second > 0,
                    "network bandwidth limit must be greater than zero"
                );
                anyhow::ensure!(
                    config.port != Some(0),
                    "network limit port must not be zero"
                );
                let matcher = config
                    .host
                    .as_deref()
                    .map(parse_allowed_entry)
                    .transpose()?;
                Ok(CompiledBandwidthLimit {
                    matcher,
                    config: config.clone(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            mode: network.mode,
            guard,
            limits,
        })
    }

    pub fn from_config(config: &impl PolicyConfig) -> anyhow::Result<Self> {
        Self::compile(config.network())
    }

    pub fn mode_str(&self) -> &'static str {
        match self.mode {
            NetworkMode::Public => "public",
            NetworkMode::NoNetwork => "no-network",
            NetworkMode::Allowlist => "allowlist",
        }
    }

    pub(crate) fn preflight(&self, request: &NetworkAccessRequest) -> Result<(), DenyReason> {
        authorize_egress(&PolicyControlController, self, request)
    }

    pub(crate) fn authorize(
        &self,
        controller: &dyn ControlController,
        request: &NetworkAccessRequest,
    ) -> Result<(), DenyReason> {
        // The compiled policy is an invariant of the data plane. An injected
        // controller may further restrict it, but must never be able to widen it.
        self.preflight(request)?;
        authorize_egress(controller, self, request)
    }

    pub(crate) fn matching_limits(
        &self,
        host: &str,
        port: Option<u16>,
        resolved_addresses: &[SocketAddr],
    ) -> Vec<NetworkBandwidthLimit> {
        self.limits
            .iter()
            .filter(|limit| {
                limit
                    .config
                    .port
                    .is_none_or(|expected| port == Some(expected))
                    && limit.matcher.as_ref().is_none_or(|matcher| {
                        host_matches(host, std::slice::from_ref(matcher))
                            || resolved_addresses.iter().any(|address| {
                                host_matches(
                                    &address.ip().to_string(),
                                    std::slice::from_ref(matcher),
                                )
                            })
                    })
            })
            .map(|limit| limit.config.clone())
            .collect()
    }
}

pub fn network_capability(network: &NetworkConfig) -> NetworkCapability {
    if network.mode == NetworkMode::NoNetwork {
        return NetworkCapability::Deny;
    }
    if network.deny_rules.is_empty() && network.limits.is_empty() {
        return match network.mode {
            NetworkMode::Public => NetworkCapability::Ambient,
            NetworkMode::NoNetwork => unreachable!(),
            NetworkMode::Allowlist => NetworkCapability::AllowList {
                hosts: network.allowed_hosts.clone(),
                rules: network.rules.clone(),
            },
        };
    }
    let mut allow = network.rules.clone();
    allow.extend(
        network
            .allowed_hosts
            .iter()
            .cloned()
            .map(|host| NetworkAccessRule {
                host,
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }),
    );
    NetworkCapability::Policy {
        default_action: match network.mode {
            NetworkMode::Public => NetworkDefaultAction::Allow,
            NetworkMode::Allowlist => NetworkDefaultAction::Deny,
            NetworkMode::NoNetwork => unreachable!(),
        },
        allow,
        deny: network.deny_rules.clone(),
        limits: network.limits.clone(),
    }
}

pub fn network_capability_from_config(config: &impl PolicyConfig) -> NetworkCapability {
    network_capability(config.network())
}

pub fn validate_network_config(network: &NetworkConfig) -> anyhow::Result<()> {
    NetworkPolicy::compile(network).map(|_| ())
}

pub fn host_from_authority(authority: &str) -> String {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return normalize_host(&rest[..end]);
    }
    if let Some((host, port)) = authority.rsplit_once(':')
        && !host.is_empty()
        && port.chars().all(|character| character.is_ascii_digit())
    {
        return normalize_host(host);
    }
    normalize_host(authority)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DenyReason {
    NoNetwork,
    AllowlistEmpty,
    NotInAllowlist,
    PortNotAllowed,
    TransportNotAllowed,
    ResolvedAddressNotAllowed,
    ExplicitDeny,
}

impl DenyReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoNetwork => "no-network",
            Self::AllowlistEmpty => "allowlist-empty",
            Self::NotInAllowlist => "not-in-allowlist",
            Self::PortNotAllowed => "port-not-allowed",
            Self::TransportNotAllowed => "transport-not-allowed",
            Self::ResolvedAddressNotAllowed => "resolved-address-not-allowed",
            Self::ExplicitDeny => "explicit-deny",
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
    if allowed {
        return Ok(());
    }
    Err(match reason {
        ControlReason::NetworkDenied => DenyReason::NoNetwork,
        ControlReason::NetworkAllowListEmpty => DenyReason::AllowlistEmpty,
        ControlReason::PortNotAllowed => DenyReason::PortNotAllowed,
        ControlReason::TransportNotAllowed => DenyReason::TransportNotAllowed,
        ControlReason::ResolvedAddressNotAllowed => DenyReason::ResolvedAddressNotAllowed,
        ControlReason::ExplicitlyDenied => DenyReason::ExplicitDeny,
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
    use persisting_agentctl::NetworkTransport;
    use persisting_agentctl::PolicyControlController;
    use proptest::prelude::*;

    fn host_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z]{1,12}\\.example\\.com").unwrap()
    }

    fn transport_strategy() -> impl Strategy<Value = NetworkTransport> {
        prop_oneof![
            Just(NetworkTransport::Http),
            Just(NetworkTransport::Https),
            Just(NetworkTransport::TcpTunnel),
        ]
    }

    proptest! {
        #[test]
        fn allowlist_preserves_explicit_network_entries(
            hosts in prop::collection::vec(host_strategy(), 0..8),
        ) {
            let capability = network_capability(&NetworkConfig {
                mode: NetworkMode::Allowlist,
                allowed_hosts: hosts.clone(),
                ..NetworkConfig::default()
            });
            prop_assert_eq!(
                capability,
                NetworkCapability::AllowList {
                    hosts,
                    rules: Vec::new(),
                }
            );
        }

        #[test]
        fn no_network_denies_every_request(
            host in host_strategy(),
            port in prop::option::of(1u16..=u16::MAX),
            transport in transport_strategy(),
        ) {
            let policy = NetworkPolicy::compile(&NetworkConfig {
                mode: NetworkMode::NoNetwork,
                ..NetworkConfig::default()
            }).unwrap();
            let request = NetworkAccessRequest {
                run_id: None,
                attempt_id: None,
                storyline_id: None,
                host,
                port,
                transport,
                resolved_ip: None,
            };
            prop_assert_eq!(
                authorize_egress(&PolicyControlController, &policy, &request),
                Err(DenyReason::NoNetwork)
            );
        }

        #[test]
        fn port_scoped_deny_rejects_only_the_denied_port(
            denied_port in 1u16..=u16::MAX,
        ) {
            let other_port = if denied_port == u16::MAX {
                1
            } else {
                denied_port + 1
            };
            let policy = NetworkPolicy::compile(&NetworkConfig {
                mode: NetworkMode::Public,
                deny_rules: vec![NetworkAccessRule {
                    host: "api.example.com".into(),
                    ports: vec![denied_port],
                    transports: Vec::new(),
                    allow_private_ips: false,
                }],
                ..NetworkConfig::default()
            }).unwrap();
            let request = |port| NetworkAccessRequest {
                run_id: None,
                attempt_id: None,
                storyline_id: None,
                host: "api.example.com".into(),
                port: Some(port),
                transport: NetworkTransport::TcpTunnel,
                resolved_ip: None,
            };
            prop_assert_eq!(
                authorize_egress(&PolicyControlController, &policy, &request(denied_port)),
                Err(DenyReason::ExplicitDeny)
            );
            prop_assert!(authorize_egress(
                &PolicyControlController,
                &policy,
                &request(other_port),
            ).is_ok());
        }

        #[test]
        fn wildcard_bandwidth_matches_subdomains_only(
            subdomain in proptest::string::string_regex("[a-z]{1,12}").unwrap(),
            port in 1u16..=u16::MAX,
        ) {
            let policy = NetworkPolicy::compile(&NetworkConfig {
                limits: vec![NetworkBandwidthLimit {
                    host: Some("*.example.com".into()),
                    port: None,
                    bytes_per_second: 1_000,
                }],
                ..NetworkConfig::default()
            }).unwrap();
            prop_assert_eq!(
                policy.matching_limits(&format!("{subdomain}.example.com"), Some(port), &[]).len(),
                1,
            );
            prop_assert!(policy.matching_limits("example.com", Some(port), &[]).is_empty());
            prop_assert!(policy.matching_limits("example.net", Some(port), &[]).is_empty());
        }

        #[test]
        fn invalid_bandwidth_limits_are_rejected(
            positive_rate in 1u64..=u64::MAX,
            invalid_kind in 0u8..3,
        ) {
            let limit = match invalid_kind {
                0 => NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 0,
                },
                1 => NetworkBandwidthLimit {
                    host: Some("https://example.com".into()),
                    port: None,
                    bytes_per_second: positive_rate,
                },
                _ => NetworkBandwidthLimit {
                    host: Some("example.com".into()),
                    port: Some(0),
                    bytes_per_second: positive_rate,
                },
            };
            let result = NetworkPolicy::compile(&NetworkConfig {
                limits: vec![limit],
                ..NetworkConfig::default()
            });
            prop_assert!(result.is_err());
        }
    }

    #[test]
    fn explicit_deny_precedes_allow_and_supports_default_allow() {
        let deny = NetworkAccessRule {
            host: "blocked.example.com".into(),
            ports: Vec::new(),
            transports: Vec::new(),
            allow_private_ips: false,
        };
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Public,
            deny_rules: vec![deny.clone()],
            ..NetworkConfig::default()
        })
        .unwrap();
        let request = |host: &str| NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            host: host.into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        };
        assert_eq!(
            authorize_egress(
                &PolicyControlController,
                &policy,
                &request("blocked.example.com")
            ),
            Err(DenyReason::ExplicitDeny)
        );
        assert!(
            authorize_egress(
                &PolicyControlController,
                &policy,
                &request("allowed.example.com")
            )
            .is_ok()
        );

        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![deny.clone()],
            deny_rules: vec![deny],
            ..NetworkConfig::default()
        })
        .unwrap();
        assert_eq!(
            authorize_egress(
                &PolicyControlController,
                &policy,
                &request("blocked.example.com")
            ),
            Err(DenyReason::ExplicitDeny)
        );
    }

    #[test]
    fn bandwidth_limits_stack_when_global_and_target_rules_match() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            limits: vec![
                NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 1_000_000,
                },
                NetworkBandwidthLimit {
                    host: Some("api.example.com".into()),
                    port: Some(443),
                    bytes_per_second: 250_000,
                },
            ],
            ..NetworkConfig::default()
        })
        .unwrap();
        let matched = policy.matching_limits("api.example.com", Some(443), &[]);
        assert_eq!(matched.len(), 2);
        assert_eq!(
            policy
                .matching_limits("api.example.com", Some(80), &[])
                .len(),
            1
        );
        assert_eq!(
            policy
                .matching_limits("other.example.com", Some(443), &[])
                .len(),
            1
        );
    }

    #[test]
    fn cidr_deny_is_applied_after_hostname_resolution() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Public,
            deny_rules: vec![NetworkAccessRule {
                host: "10.0.0.0/8".into(),
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap();
        let mut request = NetworkAccessRequest {
            run_id: None,
            attempt_id: None,
            storyline_id: None,
            host: "service.example.com".into(),
            port: Some(443),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        };
        assert!(authorize_egress(&PolicyControlController, &policy, &request).is_ok());
        request.resolved_ip = Some("10.4.5.6".parse().unwrap());
        assert_eq!(
            authorize_egress(&PolicyControlController, &policy, &request),
            Err(DenyReason::ExplicitDeny)
        );
    }

    #[test]
    fn cidr_bandwidth_limit_matches_resolved_hostname_address() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            limits: vec![NetworkBandwidthLimit {
                host: Some("10.0.0.0/8".into()),
                port: Some(443),
                bytes_per_second: 1_000,
            }],
            ..NetworkConfig::default()
        })
        .unwrap();
        let resolved = ["10.4.5.6:443".parse().unwrap()];
        assert_eq!(
            policy
                .matching_limits("service.internal", Some(443), &resolved)
                .len(),
            1
        );
        assert!(
            policy
                .matching_limits(
                    "service.internal",
                    Some(443),
                    &["192.168.1.2:443".parse().unwrap()],
                )
                .is_empty()
        );
    }

    #[test]
    fn public_and_no_network_modes_reject_allow_entries() {
        for mode in [NetworkMode::Public, NetworkMode::NoNetwork] {
            assert!(
                NetworkPolicy::compile(&NetworkConfig {
                    mode,
                    rules: vec![NetworkAccessRule {
                        host: "api.example.com".into(),
                        ports: vec![443],
                        transports: Vec::new(),
                        allow_private_ips: false,
                    }],
                    ..NetworkConfig::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn public_validation_rejects_invalid_bandwidth_limits() {
        assert!(
            validate_network_config(&NetworkConfig {
                limits: vec![NetworkBandwidthLimit {
                    host: None,
                    port: None,
                    bytes_per_second: 0,
                }],
                ..NetworkConfig::default()
            })
            .is_err()
        );
    }
}
