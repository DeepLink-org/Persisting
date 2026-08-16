//! Reusable client for pVisor's cooperative, low-frequency AgentCtl protocol.

use crate::{
    AgentCheckpointQuiesced, AgentClientRole, AgentDirective, AgentHeartbeatAck, AgentHello,
    AgentLifecycleState, AgentOperationBegin, AgentOperationComplete, AgentProcessRegistration,
    AgentRequest, AgentRequestBody, AgentResponse, AgentResponseBody, AgentWelcome,
    AGENTCTL_ENDPOINT_ENV, AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_TOKEN_ENV, AGENTCTL_TRANSPORT_ENV,
    AGENTCTL_VERSION, AGENTCTL_VERSION_ENV, LEGACY_AGENT_ABI_ENDPOINT_ENV,
    LEGACY_AGENT_ABI_TOKEN_ENV, LEGACY_AGENT_ABI_TRANSPORT_ENV, LEGACY_AGENT_ABI_VERSION_ENV,
};
use anyhow::{bail, Context};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub struct AgentCtlClientConfig {
    pub endpoint: PathBuf,
    pub auth_token: String,
    pub client_id: String,
    pub role: AgentClientRole,
    pub agent_name: String,
}

impl AgentCtlClientConfig {
    pub fn from_current_environment(
        client_id: impl Into<String>,
        role: AgentClientRole,
        agent_name: impl Into<String>,
    ) -> anyhow::Result<Option<Self>> {
        Self::from_environment(&std::env::vars().collect(), client_id, role, agent_name)
    }

    pub fn from_environment(
        environment: &BTreeMap<String, String>,
        client_id: impl Into<String>,
        role: AgentClientRole,
        agent_name: impl Into<String>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(endpoint) = environment
            .get(AGENTCTL_ENDPOINT_ENV)
            .or_else(|| environment.get(LEGACY_AGENT_ABI_ENDPOINT_ENV))
        else {
            return Ok(None);
        };
        let transport = environment
            .get(AGENTCTL_TRANSPORT_ENV)
            .or_else(|| environment.get(LEGACY_AGENT_ABI_TRANSPORT_ENV))
            .map(String::as_str)
            .unwrap_or("unix");
        if transport != "unix" {
            bail!("unsupported AgentCtl transport {transport:?}");
        }
        let version = environment
            .get(AGENTCTL_VERSION_ENV)
            .or_else(|| environment.get(LEGACY_AGENT_ABI_VERSION_ENV))
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
        Ok(Some(Self {
            endpoint: endpoint.into(),
            auth_token: environment
                .get(AGENTCTL_TOKEN_ENV)
                .or_else(|| environment.get(LEGACY_AGENT_ABI_TOKEN_ENV))
                .context("AgentCtl endpoint is present without an auth token")?
                .clone(),
            client_id: client_id.into(),
            role,
            agent_name: agent_name.into(),
        }))
    }
}

pub struct AgentCtlClient {
    config: AgentCtlClientConfig,
    session_id: Option<String>,
}

impl AgentCtlClient {
    pub fn new(config: AgentCtlClientConfig) -> Self {
        Self {
            config,
            session_id: None,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn connect(&mut self) -> anyhow::Result<AgentWelcome> {
        let response = exchange(
            &self.config.endpoint,
            &AgentRequest::hello(AgentHello {
                auth_token: self.config.auth_token.clone(),
                client_id: self.config.client_id.clone(),
                role: self.config.role,
                agent_name: self.config.agent_name.clone(),
            }),
        )?;
        match response.body {
            AgentResponseBody::Welcome(welcome) => {
                self.session_id = Some(welcome.session_id.clone());
                Ok(welcome)
            }
            body => unexpected("welcome", body),
        }
    }

    pub fn heartbeat(
        &mut self,
        lifecycle: AgentLifecycleState,
    ) -> anyhow::Result<AgentHeartbeatAck> {
        match self.request(AgentRequestBody::Heartbeat(lifecycle))? {
            AgentResponseBody::Heartbeat(ack) => Ok(ack),
            body => unexpected("heartbeat acknowledgement", body),
        }
    }

    pub fn register_process(&mut self, value: AgentProcessRegistration) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::RegisterProcess(value))?)
    }

    pub fn checkpoint_quiesced(&mut self, value: AgentCheckpointQuiesced) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::CheckpointQuiesced(value))?)
    }

    pub fn begin_operation(&mut self, value: AgentOperationBegin) -> anyhow::Result<u64> {
        match self.request(AgentRequestBody::EffectBegin(value))? {
            AgentResponseBody::OperationAccepted { sequence } => Ok(sequence),
            body => unexpected("effect acceptance", body),
        }
    }

    pub fn complete_operation(&mut self, value: AgentOperationComplete) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::EffectComplete(value))?)
    }

    #[deprecated(note = "use begin_operation; AgentCtl reports are cooperative declarations")]
    pub fn begin_effect(&mut self, value: AgentOperationBegin) -> anyhow::Result<u64> {
        self.begin_operation(value)
    }

    #[deprecated(note = "use complete_operation; AgentCtl reports are cooperative declarations")]
    pub fn complete_effect(&mut self, value: AgentOperationComplete) -> anyhow::Result<()> {
        self.complete_operation(value)
    }

    fn request(&mut self, body: AgentRequestBody) -> anyhow::Result<AgentResponseBody> {
        let session_id = self
            .session_id
            .clone()
            .context("AgentCtl client is not connected")?;
        Ok(exchange(
            &self.config.endpoint,
            &AgentRequest::authenticated(session_id, body),
        )?
        .body)
    }
}

pub fn checkpoint_directive(ack: &AgentHeartbeatAck) -> Option<(&str, u64)> {
    match &ack.directive {
        AgentDirective::Quiesce { checkpoint_id, .. } => {
            Some((checkpoint_id.as_str(), ack.directive_seq))
        }
        AgentDirective::Continue | AgentDirective::Shutdown { .. } => None,
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
    if let AgentResponseBody::Error { message } = &response.body {
        bail!("AgentCtl: {message}");
    }
    Ok(response)
}

fn expect_ack(body: AgentResponseBody) -> anyhow::Result<()> {
    match body {
        AgentResponseBody::Ack => Ok(()),
        body => unexpected("acknowledgement", body),
    }
}

fn unexpected<T>(expected: &str, body: AgentResponseBody) -> anyhow::Result<T> {
    bail!("expected AgentCtl {expected}, got {body:?}")
}
