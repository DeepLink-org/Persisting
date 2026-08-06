//! Resolve a logical target once, authorize each concrete address, and return
//! only addresses that the connector is permitted to use.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use persisting_control::ControlController;
use persisting_control::NetworkAccessRequest;
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

pub(crate) async fn authorize_target(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
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

    authorize_resolved_target(controller, policy, request, resolved)
}

fn authorize_resolved_target(
    controller: &dyn ControlController,
    policy: &NetworkPolicy,
    mut request: NetworkAccessRequest,
    resolved: impl IntoIterator<Item = SocketAddr>,
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
        match policy.authorize(controller, &request) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{NetworkConfig, NetworkMode};
    use persisting_control::{
        ControlReason, ControlRequest, ControlTransition, PolicyControlController,
    };
    use persisting_control::{NetworkAccessRule, NetworkTransport};

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
            if let ControlRequest::Network { request, .. } = &request {
                if request.resolved_ip == Some("93.184.216.34".parse().unwrap()) {
                    return ControlTransition::denied(ControlReason::ExplicitlyDenied);
                }
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
            if let ControlRequest::Network { request, .. } = request {
                if request.host == "controller-denied.invalid" {
                    return ControlTransition::denied(ControlReason::ExplicitlyDenied);
                }
            }
            ControlTransition::allowed(ControlReason::AmbientNetwork)
        }
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
}
