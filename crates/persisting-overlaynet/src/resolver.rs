//! Resolve a logical target once, authorize each concrete address, and return
//! only addresses that the connector is permitted to use.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use persisting_agentctl::ControlController;
use persisting_agentctl::NetworkAccessRequest;
use tokio::net::lookup_host;
use tokio::time::timeout;

use crate::policy::{DenyReason, NetworkPolicy};

const DNS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct AuthorizedTarget {
    pub host: String,
    pub addresses: Vec<SocketAddr>,
}

#[derive(Debug)]
pub(crate) enum TargetAuthorizationError {
    Denied(DenyReason),
    Resolve(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolvedAddressPolicy {
    Strict,
    /// Accept an opaque address returned by a host fake-IP DNS/TUN connector.
    /// The logical hostname is re-authorized and IP literals never qualify.
    HostConnectorAliases,
}

pub(crate) async fn authorize_target(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    request: NetworkAccessRequest,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    authorize_target_with_policy(controller, policy, request, ResolvedAddressPolicy::Strict).await
}

pub(crate) async fn authorize_target_with_policy(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
    resolved_address_policy: ResolvedAddressPolicy,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    request.resolved_ip = None;
    policy
        .authorize(controller, &request)
        .map_err(TargetAuthorizationError::Denied)?;

    let port = request.port.ok_or_else(|| {
        TargetAuthorizationError::Resolve(anyhow::anyhow!(
            "network target `{}` has no effective port",
            request.host
        ))
    })?;
    let resolved = timeout(DNS_TIMEOUT, lookup_host((request.host.as_str(), port)))
        .await
        .map_err(|_| {
            TargetAuthorizationError::Resolve(anyhow::anyhow!(
                "DNS resolution for `{}` timed out",
                request.host
            ))
        })?
        .map_err(|error| {
            TargetAuthorizationError::Resolve(anyhow::anyhow!(
                "resolve `{}`: {error}",
                request.host
            ))
        })?
        .collect::<Vec<_>>();

    authorize_resolved_target_with_policy(
        controller,
        policy,
        request,
        resolved,
        resolved_address_policy,
    )
}

fn authorize_resolved_target_with_policy(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
    resolved: impl IntoIterator<Item = SocketAddr>,
    resolved_address_policy: ResolvedAddressPolicy,
) -> Result<AuthorizedTarget, TargetAuthorizationError> {
    let mut seen = HashSet::new();
    let mut addresses = Vec::new();
    let mut denied = DenyReason::ResolvedAddressNotAllowed;
    let mut resolved_any = false;
    for address in resolved {
        resolved_any = true;
        if !seen.insert(address) {
            continue;
        }
        request.resolved_ip = Some(address.ip());
        let authorization = policy.authorize(controller, &request).or_else(|reason| {
            if reason != DenyReason::ResolvedAddressNotAllowed
                || resolved_address_policy != ResolvedAddressPolicy::HostConnectorAliases
                || !is_host_connector_alias(&request.host, address.ip())
            {
                return Err(reason);
            }
            let mut logical_request = request.clone();
            logical_request.resolved_ip = None;
            policy.authorize(controller, &logical_request)
        });
        match authorization {
            Ok(()) => addresses.push(address),
            Err(reason) => denied = reason,
        }
    }
    if !resolved_any {
        return Err(TargetAuthorizationError::Resolve(anyhow::anyhow!(
            "DNS resolution for `{}` returned no addresses",
            request.host
        )));
    }
    if addresses.is_empty() {
        return Err(TargetAuthorizationError::Denied(denied));
    }
    Ok(AuthorizedTarget {
        host: request.host,
        addresses,
    })
}

/// The benchmarking range is commonly used as an opaque fake-IP namespace by
/// host DNS/TUN connectors. It is never a connector alias for an IP-literal
/// request, so a guest cannot use this exception as direct egress.
pub(crate) fn is_host_connector_alias(host: &str, address: IpAddr) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return false;
    }
    let IpAddr::V4(address) = address else {
        return false;
    };
    let octets = address.octets();
    octets[0] == 198 && (octets[1] == 18 || octets[1] == 19)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{NetworkConfig, NetworkMode};
    use persisting_agentctl::{
        ControlReason, ControlRequest, ControlTransition, PolicyControlController,
    };
    use persisting_agentctl::{NetworkAccessRule, NetworkTransport};
    use proptest::prelude::*;

    fn request(host: &str) -> NetworkAccessRequest {
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

    fn authorize_resolved_target(
        controller: &dyn ControlController,
        policy: &NetworkPolicy,
        request: NetworkAccessRequest,
        resolved: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<AuthorizedTarget, TargetAuthorizationError> {
        authorize_resolved_target_with_policy(
            controller,
            policy,
            request,
            resolved,
            ResolvedAddressPolicy::Strict,
        )
    }

    fn host_allowlist(host: &str) -> NetworkPolicy {
        NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![NetworkAccessRule {
                host: host.into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn filters_private_addresses_and_deduplicates_public_results() {
        let policy = host_allowlist("api.example.com");
        let public: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let private: SocketAddr = "127.0.0.1:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [private, public, public],
        )
        .unwrap();
        assert_eq!(target.addresses, [public]);
    }

    #[test]
    fn rejects_when_every_resolved_address_is_private() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [
                "127.0.0.1:443".parse().unwrap(),
                "[::1]:443".parse().unwrap(),
            ],
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(
                DenyReason::ResolvedAddressNotAllowed
            ))
        ));
    }

    #[test]
    fn strict_resolution_rejects_host_connector_aliases() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["198.18.0.42:443".parse().unwrap()],
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(
                DenyReason::ResolvedAddressNotAllowed
            ))
        ));
    }

    #[test]
    fn vm_resolution_accepts_an_authorized_host_connector_alias() {
        let policy = host_allowlist("api.example.com");
        let alias: SocketAddr = "198.18.0.42:443".parse().unwrap();
        let target = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [alias],
            ResolvedAddressPolicy::HostConnectorAliases,
        )
        .unwrap();
        assert_eq!(target.addresses, [alias]);
    }

    #[test]
    fn explicit_connector_range_deny_is_not_bypassed() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::Allowlist,
            rules: vec![NetworkAccessRule {
                host: "api.example.com".into(),
                ports: vec![443],
                transports: vec![NetworkTransport::TcpTunnel],
                allow_private_ips: false,
            }],
            deny_rules: vec![NetworkAccessRule {
                host: "198.18.0.0/15".into(),
                ports: Vec::new(),
                transports: Vec::new(),
                allow_private_ips: false,
            }],
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_resolved_target_with_policy(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["198.18.0.42:443".parse().unwrap()],
            ResolvedAddressPolicy::HostConnectorAliases,
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    #[test]
    fn connector_aliases_require_a_logical_hostname() {
        assert!(is_host_connector_alias(
            "api.example.com",
            "198.18.0.42".parse().unwrap()
        ));
        assert!(!is_host_connector_alias(
            "198.18.0.42",
            "198.18.0.42".parse().unwrap()
        ));
        assert!(!is_host_connector_alias(
            "api.example.com",
            "93.184.216.34".parse().unwrap()
        ));
    }

    #[test]
    fn explicit_cidr_deny_filters_only_matching_results() {
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
        let allowed: SocketAddr = "93.184.216.34:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            ["10.0.0.1:443".parse().unwrap(), allowed],
        )
        .unwrap();
        assert_eq!(target.addresses, [allowed]);
    }

    #[test]
    fn empty_resolution_is_an_upstream_error_not_a_policy_denial() {
        let policy = host_allowlist("api.example.com");
        let result = authorize_resolved_target(
            &PolicyControlController,
            &policy,
            request("api.example.com"),
            [],
        );
        assert!(matches!(result, Err(TargetAuthorizationError::Resolve(_))));
    }

    struct PerAddressController;

    impl ControlController for PerAddressController {
        fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
            if let ControlRequest::Network { request, .. } = &request
                && request.resolved_ip == Some("93.184.216.34".parse().unwrap())
            {
                return ControlTransition::denied(ControlReason::ExplicitlyDenied);
            }
            PolicyControlController.authorize(request)
        }
    }

    #[test]
    fn injected_controller_can_veto_one_concrete_address() {
        let policy = host_allowlist("api.example.com");
        let retained: SocketAddr = "1.1.1.1:443".parse().unwrap();
        let target = authorize_resolved_target(
            &PerAddressController,
            &policy,
            request("api.example.com"),
            ["93.184.216.34:443".parse().unwrap(), retained],
        )
        .unwrap();
        assert_eq!(target.addresses, [retained]);
    }

    struct DenyHostController;

    impl ControlController for DenyHostController {
        fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
            if let ControlRequest::Network { request, .. } = request
                && request.host == "controller-denied.invalid"
            {
                return ControlTransition::denied(ControlReason::ExplicitlyDenied);
            }
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
    }

    #[test]
    fn connector_aliases_do_not_bypass_the_logical_controller_decision() {
        let policy = host_allowlist("controller-denied.invalid");
        let result = authorize_resolved_target_with_policy(
            &DenyHostController,
            &policy,
            request("controller-denied.invalid"),
            ["198.18.0.42:443".parse().unwrap()],
            ResolvedAddressPolicy::HostConnectorAliases,
        );
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    #[tokio::test]
    async fn injected_controller_denies_before_dns_resolution() {
        let policy = NetworkPolicy::compile(&NetworkConfig::default()).unwrap();
        let result = authorize_target(
            &DenyHostController,
            &policy,
            request("controller-denied.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::ExplicitDeny))
        ));
    }

    struct AllowEverythingController;

    impl ControlController for AllowEverythingController {
        fn authorize(&self, _request: ControlRequest<'_>) -> ControlTransition {
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
    }

    #[tokio::test]
    async fn injected_controller_cannot_widen_compiled_policy() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_target(
            &AllowEverythingController,
            &policy,
            request("must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NoNetwork))
        ));
    }

    #[tokio::test]
    async fn static_denial_happens_before_dns_resolution() {
        let policy = NetworkPolicy::compile(&NetworkConfig {
            mode: NetworkMode::NoNetwork,
            ..NetworkConfig::default()
        })
        .unwrap();
        let result = authorize_target(
            &PolicyControlController,
            &policy,
            request("must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NoNetwork))
        ));

        let policy = host_allowlist("allowed.example.com");
        let result = authorize_target(
            &PolicyControlController,
            &policy,
            request("unlisted.must-not-resolve.invalid"),
        )
        .await;
        assert!(matches!(
            result,
            Err(TargetAuthorizationError::Denied(DenyReason::NotInAllowlist))
        ));
    }

    fn connector_alias_strategy() -> impl Strategy<Value = IpAddr> {
        (18u8..=19, any::<u8>(), any::<u8>())
            .prop_map(|(second, third, fourth)| [198, second, third, fourth].into())
    }

    proptest! {
        #[test]
        fn connector_alias_range_is_recognized_for_logical_hosts_only(
            address in connector_alias_strategy(),
        ) {
            prop_assert!(is_host_connector_alias("api.example.com", address));
            prop_assert!(!is_host_connector_alias(&address.to_string(), address));
        }

        #[test]
        fn addresses_outside_connector_range_are_never_aliases(
            first in any::<u8>(),
            second in any::<u8>(),
            third in any::<u8>(),
            fourth in any::<u8>(),
        ) {
            prop_assume!(first != 198 || (second != 18 && second != 19));
            let address = IpAddr::from([first, second, third, fourth]);
            prop_assert!(!is_host_connector_alias("api.example.com", address));
        }

        #[test]
        fn authorized_results_are_public_deduplicated_and_first_seen_ordered(
            indexes in prop::collection::vec(0usize..6, 1..32),
        ) {
            let candidates: [SocketAddr; 6] = [
                "1.1.1.1:443".parse().unwrap(),
                "8.8.8.8:443".parse().unwrap(),
                "9.9.9.9:443".parse().unwrap(),
                "127.0.0.1:443".parse().unwrap(),
                "10.0.0.1:443".parse().unwrap(),
                "192.168.0.1:443".parse().unwrap(),
            ];
            let resolved = indexes.iter().map(|index| candidates[*index]).collect::<Vec<_>>();
            let mut expected = Vec::new();
            for address in &resolved {
                if !address.ip().is_loopback()
                    && !matches!(address.ip(), IpAddr::V4(ip) if ip.is_private())
                    && !expected.contains(address)
                {
                    expected.push(*address);
                }
            }

            let policy = host_allowlist("api.example.com");
            let result = authorize_resolved_target(
                &PolicyControlController,
                &policy,
                request("api.example.com"),
                resolved,
            );
            if expected.is_empty() {
                prop_assert!(matches!(
                    result,
                    Err(TargetAuthorizationError::Denied(DenyReason::ResolvedAddressNotAllowed))
                ));
            } else {
                prop_assert_eq!(result.unwrap().addresses, expected);
            }
        }

        #[test]
        fn alias_policy_accepts_the_entire_benchmarking_range(
            address in connector_alias_strategy(),
        ) {
            let policy = host_allowlist("api.example.com");
            let socket = SocketAddr::new(address, 443);
            let target = authorize_resolved_target_with_policy(
                &PolicyControlController,
                &policy,
                request("api.example.com"),
                [socket],
                ResolvedAddressPolicy::HostConnectorAliases,
            ).unwrap();
            prop_assert_eq!(target.addresses, [socket]);
        }
    }
}
