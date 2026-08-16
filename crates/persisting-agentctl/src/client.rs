//! Synchronous client for pVisor's cooperative, low-frequency AgentCtl protocol.

use crate::{
    AgentDirective, AgentErrorCode, AgentRequest, AgentResponse, AgentState, AGENTCTL_ENDPOINT_ENV,
    AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_TOKEN_ENV, AGENTCTL_TRANSPORT_ENV, AGENTCTL_VERSION,
    AGENTCTL_VERSION_ENV,
};
use anyhow::{bail, Context};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

/// Configuration discovered from one Run's injected AgentCtl environment.
pub struct AgentCtlClientConfig {
    /// Run-local Unix socket path.
    pub endpoint: PathBuf,
    /// Run-scoped authentication token.
    pub token: String,
    /// Stable non-empty identity for this runtime client.
    pub client_id: String,
}

impl AgentCtlClientConfig {
    /// Discover AgentCtl from the current process environment.
    pub fn from_current_environment(client_id: impl Into<String>) -> anyhow::Result<Option<Self>> {
        Self::from_environment(&std::env::vars().collect(), client_id)
    }

    /// Discover AgentCtl from an explicit environment projection.
    pub fn from_environment(
        environment: &BTreeMap<String, String>,
        client_id: impl Into<String>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(endpoint) = environment.get(AGENTCTL_ENDPOINT_ENV) else {
            return Ok(None);
        };
        let transport = environment
            .get(AGENTCTL_TRANSPORT_ENV)
            .map(String::as_str)
            .unwrap_or("unix");
        if transport != "unix" {
            bail!("unsupported AgentCtl transport {transport:?}");
        }
        let version = environment
            .get(AGENTCTL_VERSION_ENV)
            .context("AgentCtl endpoint is present without a version")?
            .parse::<u32>()
            .context("parse AgentCtl version")?;
        if version != AGENTCTL_VERSION {
            bail!(
                "AgentCtl version mismatch: client expects {}, environment advertises {}",
                AGENTCTL_VERSION,
                version
            );
        }
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            bail!("AgentCtl client_id must be non-empty");
        }
        Ok(Some(Self {
            endpoint: endpoint.into(),
            token: environment
                .get(AGENTCTL_TOKEN_ENV)
                .context("AgentCtl endpoint is present without an auth token")?
                .clone(),
            client_id,
        }))
    }
}

/// A machine-readable error returned by the AgentCtl server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCtlResponseError {
    /// Stable category used for client control flow.
    pub code: AgentErrorCode,
    /// Human-readable diagnostic context.
    pub message: String,
}

impl fmt::Display for AgentCtlResponseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = match self.code {
            AgentErrorCode::InvalidRequest => "invalid_request",
            AgentErrorCode::Unauthorized => "unauthorized",
            AgentErrorCode::VersionMismatch => "version_mismatch",
            AgentErrorCode::Conflict => "conflict",
        };
        write!(formatter, "AgentCtl {code}: {}", self.message)
    }
}

impl std::error::Error for AgentCtlResponseError {}

/// One runtime client's AgentCtl Session.
pub struct AgentCtlClient {
    config: AgentCtlClientConfig,
    session_id: Option<String>,
    sync_interval_ms: Option<u64>,
}

impl AgentCtlClient {
    /// Construct a disconnected client.
    pub fn new(config: AgentCtlClientConfig) -> Self {
        Self {
            config,
            session_id: None,
            sync_interval_ms: None,
        }
    }

    /// Return the current Session identifier after a successful connection.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Return the server-recommended synchronization interval after connection.
    pub fn sync_interval_ms(&self) -> Option<u64> {
        self.sync_interval_ms
    }

    /// Authenticate, create a Session, and return the current directive.
    pub fn connect(&mut self) -> anyhow::Result<AgentDirective> {
        let response = exchange(
            &self.config.endpoint,
            &AgentRequest::Hello {
                version: AGENTCTL_VERSION,
                token: self.config.token.clone(),
                client_id: self.config.client_id.clone(),
            },
        )?;
        match response {
            AgentResponse::Welcome {
                session_id,
                sync_interval_ms,
                directive,
            } => {
                self.session_id = Some(session_id);
                self.sync_interval_ms = Some(sync_interval_ms);
                Ok(directive)
            }
            response => unexpected("welcome", response),
        }
    }

    /// Report cooperative state and return pVisor's current directive.
    pub fn sync(&mut self, state: AgentState) -> anyhow::Result<AgentDirective> {
        let session_id = self
            .session_id
            .clone()
            .context("AgentCtl client is not connected")?;
        match exchange(
            &self.config.endpoint,
            &AgentRequest::Sync {
                version: AGENTCTL_VERSION,
                session_id,
                state,
            },
        )? {
            AgentResponse::Synced { directive } => Ok(directive),
            response => unexpected("sync acknowledgement", response),
        }
    }
}

fn exchange(path: &Path, request: &AgentRequest) -> anyhow::Result<AgentResponse> {
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .with_context(|| format!("connect AgentCtl endpoint {}", path.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut frame = Vec::new();
    std::io::BufReader::new(stream)
        .by_ref()
        .take((AGENTCTL_MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if frame.len() > AGENTCTL_MAX_FRAME_BYTES {
        bail!("AgentCtl response exceeds {AGENTCTL_MAX_FRAME_BYTES} bytes");
    }
    let response: AgentResponse = serde_json::from_slice(&frame)?;
    if let AgentResponse::Error { code, message } = response {
        return Err(AgentCtlResponseError { code, message }.into());
    }
    Ok(response)
}

fn unexpected<T>(expected: &str, response: AgentResponse) -> anyhow::Result<T> {
    bail!("expected AgentCtl {expected}, got {response:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment() -> BTreeMap<String, String> {
        BTreeMap::from([
            (AGENTCTL_ENDPOINT_ENV.into(), "/tmp/agentctl.sock".into()),
            (AGENTCTL_TOKEN_ENV.into(), "secret".into()),
            (AGENTCTL_VERSION_ENV.into(), "1".into()),
            (AGENTCTL_TRANSPORT_ENV.into(), "unix".into()),
        ])
    }

    #[test]
    fn config_reads_only_agentctl_v1_environment() {
        let config = AgentCtlClientConfig::from_environment(&environment(), "worker-1")
            .unwrap()
            .unwrap();

        assert_eq!(config.client_id, "worker-1");
        assert_eq!(config.token, "secret");
        assert_eq!(config.endpoint, PathBuf::from("/tmp/agentctl.sock"));
    }

    #[test]
    fn legacy_agent_abi_environment_is_not_discovered() {
        let environment = BTreeMap::from([(
            "PERSISTING_AGENT_ABI_ENDPOINT".into(),
            "/tmp/legacy.sock".into(),
        )]);

        assert!(
            AgentCtlClientConfig::from_environment(&environment, "worker-1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn response_error_keeps_machine_code_and_human_message() {
        let error = AgentCtlResponseError {
            code: AgentErrorCode::Conflict,
            message: "busy".into(),
        };

        assert_eq!(error.code, AgentErrorCode::Conflict);
        assert_eq!(error.message, "busy");
        assert_eq!(error.to_string(), "AgentCtl conflict: busy");
    }
}
