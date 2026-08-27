//! VM egress authorization, resolution, and rate limiting.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use crate::bandwidth::{BandwidthRegistry, BandwidthSession};
use crate::policy::{DenyReason, NetworkPolicy};
use crate::resolver::{
    ResolvedAddressPolicy, TargetAuthorizationError, authorize_target_with_policy,
};
use persisting_agentctl::{
    AttemptId, ControlController, NetworkAccessRequest, NetworkTransport, RunId, StorylineId,
};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default)]
pub struct EgressContext {
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub storyline_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum EgressError {
    #[error("egress denied: {0:?}")]
    Denied(DenyReason),
    #[error("egress resolution failed: {0:#}")]
    Resolve(anyhow::Error),
    #[error("egress connection to {host}:{port} timed out")]
    ConnectTimeout { host: String, port: u16 },
    #[error("egress connection to {host}:{port} failed: {source}")]
    Connect {
        host: String,
        port: u16,
        #[source]
        source: std::io::Error,
    },
}

/// Attempt-scoped authorization services for transparent VM flows.
///
/// The compiled policy remains an invariant and an injected controller may
/// only narrow it. The injected registry shares identical bandwidth buckets
/// with the explicit proxy running in the same Attempt.
#[derive(Clone)]
pub struct EgressRuntime {
    policy: NetworkPolicy,
    controller: Arc<dyn ControlController>,
    bandwidth: BandwidthRegistry,
}

impl std::fmt::Debug for EgressRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressRuntime")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl EgressRuntime {
    pub fn with_bandwidth_registry(
        policy: NetworkPolicy,
        controller: Arc<dyn ControlController>,
        bandwidth: BandwidthRegistry,
    ) -> Self {
        Self {
            policy,
            controller,
            bandwidth,
        }
    }

    pub(crate) async fn authorize_tcp(
        &self,
        context: &EgressContext,
        host: &str,
        port: u16,
    ) -> Result<(Vec<SocketAddr>, BandwidthSession), EgressError> {
        let request = NetworkAccessRequest {
            run_id: context.run_id.clone().map(RunId),
            attempt_id: context.attempt_id.clone().map(AttemptId),
            storyline_id: context.storyline_id.clone().map(StorylineId),
            host: host.to_owned(),
            port: Some(port),
            transport: NetworkTransport::TcpTunnel,
            resolved_ip: None,
        };
        let authorized = authorize_target_with_policy(
            self.controller.as_ref(),
            &self.policy,
            request,
            ResolvedAddressPolicy::HostConnectorAliases,
        )
        .await
        .map_err(|error| match error {
            TargetAuthorizationError::Denied(reason) => EgressError::Denied(reason),
            TargetAuthorizationError::Resolve(error) => EgressError::Resolve(error),
        })?;
        let bandwidth = self
            .bandwidth
            .session(
                self.policy
                    .matching_limits(host, Some(port), &authorized.addresses),
            )
            .await;
        Ok((authorized.addresses, bandwidth))
    }
}

pub(crate) async fn connect_tcp_addresses(
    addresses: &[SocketAddr],
    host: &str,
    port: u16,
) -> Result<TcpStream, EgressError> {
    let mut last_error = None;
    timeout(CONNECT_TIMEOUT, async {
        for address in addresses {
            match TcpStream::connect(address).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no authorized address",
            )
        }))
    })
    .await
    .map_err(|_| EgressError::ConnectTimeout {
        host: host.to_owned(),
        port,
    })?
    .map_err(|source| EgressError::Connect {
        host: host.to_owned(),
        port,
        source,
    })
}
