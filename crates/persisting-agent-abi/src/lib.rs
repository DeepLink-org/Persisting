//! Reusable client for pVisor's versioned, low-frequency Agent ABI.

use anyhow::{bail, Context};
use persisting_pvisor::{
    AgentCheckpointQuiesced, AgentClientRole, AgentDirective, AgentEffectBegin,
    AgentEffectComplete, AgentHeartbeatAck, AgentHello, AgentLifecycleState,
    AgentProcessRegistration, AgentRequest, AgentRequestBody, AgentResponse, AgentResponseBody,
    AgentWelcome, AGENT_ABI_ENDPOINT_ENV, AGENT_ABI_MAX_FRAME_BYTES, AGENT_ABI_TOKEN_ENV,
    AGENT_ABI_TRANSPORT_ENV, AGENT_ABI_VERSION, AGENT_ABI_VERSION_ENV,
};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub struct AgentAbiClientConfig {
    pub endpoint: PathBuf,
    pub auth_token: String,
    pub client_id: String,
    pub role: AgentClientRole,
    pub agent_name: String,
}

impl AgentAbiClientConfig {
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
        let Some(endpoint) = environment.get(AGENT_ABI_ENDPOINT_ENV) else {
            return Ok(None);
        };
        let transport = environment
            .get(AGENT_ABI_TRANSPORT_ENV)
            .map(String::as_str)
            .unwrap_or("unix");
        if transport != "unix" {
            bail!("unsupported Agent ABI transport {transport:?}");
        }
        let version = environment
            .get(AGENT_ABI_VERSION_ENV)
            .context("Agent ABI endpoint is present without a version")?
            .parse::<u32>()
            .context("parse Agent ABI version")?;
        if version != AGENT_ABI_VERSION {
            bail!(
                "Agent ABI version mismatch: client expects {}, environment advertises {}",
                AGENT_ABI_VERSION,
                version
            );
        }
        Ok(Some(Self {
            endpoint: endpoint.into(),
            auth_token: environment
                .get(AGENT_ABI_TOKEN_ENV)
                .context("Agent ABI endpoint is present without an auth token")?
                .clone(),
            client_id: client_id.into(),
            role,
            agent_name: agent_name.into(),
        }))
    }
}

pub struct AgentAbiClient {
    config: AgentAbiClientConfig,
    session_id: Option<String>,
}

impl AgentAbiClient {
    pub fn new(config: AgentAbiClientConfig) -> Self {
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

    pub fn begin_effect(&mut self, value: AgentEffectBegin) -> anyhow::Result<u64> {
        match self.request(AgentRequestBody::EffectBegin(value))? {
            AgentResponseBody::EffectAccepted { sequence } => Ok(sequence),
            body => unexpected("effect acceptance", body),
        }
    }

    pub fn complete_effect(&mut self, value: AgentEffectComplete) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::EffectComplete(value))?)
    }

    fn request(&mut self, body: AgentRequestBody) -> anyhow::Result<AgentResponseBody> {
        let session_id = self
            .session_id
            .clone()
            .context("Agent ABI client is not connected")?;
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
        .with_context(|| format!("connect Agent ABI endpoint {}", path.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut frame = Vec::new();
    std::io::BufReader::new(stream)
        .by_ref()
        .take((AGENT_ABI_MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if frame.len() > AGENT_ABI_MAX_FRAME_BYTES {
        bail!("Agent ABI response exceeds {AGENT_ABI_MAX_FRAME_BYTES} bytes");
    }
    let response: AgentResponse = serde_json::from_slice(&frame)?;
    if let AgentResponseBody::Error { message } = &response.body {
        bail!("Agent ABI: {message}");
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
    bail!("expected Agent ABI {expected}, got {body:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_control::{AttemptId, RunId};
    use persisting_pvisor::{AgentAbiServer, AgentEffectOutcome};

    #[test]
    fn sdk_drives_process_effect_and_quiescence_lifecycle() {
        let server =
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let config = AgentAbiClientConfig::from_environment(
            &server.environment(),
            "agent-1",
            AgentClientRole::Agent,
            "example-agent",
        )
        .unwrap()
        .unwrap();
        let mut client = AgentAbiClient::new(config);
        client.connect().unwrap();
        client
            .register_process(AgentProcessRegistration {
                pid: 7,
                role: "agent".into(),
                executable: Some("example-agent".into()),
            })
            .unwrap();
        client
            .begin_effect(AgentEffectBegin {
                effect_id: "effect-1".into(),
                kind: "tool.call".into(),
                request_digest: "sha256:abc".into(),
                idempotency_key: Some("idem-1".into()),
            })
            .unwrap();
        client
            .complete_effect(AgentEffectComplete {
                effect_id: "effect-1".into(),
                outcome: AgentEffectOutcome::Committed,
            })
            .unwrap();
        let directive_seq = server.control().request_quiesce("checkpoint-1", None);
        let ack = client.heartbeat(AgentLifecycleState::Quiescing).unwrap();
        assert_eq!(
            checkpoint_directive(&ack),
            Some(("checkpoint-1", directive_seq))
        );
        client
            .checkpoint_quiesced(AgentCheckpointQuiesced {
                checkpoint_id: "checkpoint-1".into(),
                directive_seq,
            })
            .unwrap();
        let snapshot = server.control().snapshot();
        assert_eq!(snapshot.clients[0].client_id, "agent-1");
        assert_eq!(snapshot.processes[0].registration.pid, 7);
        assert_eq!(
            snapshot.effects[0].completion.as_ref().unwrap().outcome,
            AgentEffectOutcome::Committed
        );
    }
}
