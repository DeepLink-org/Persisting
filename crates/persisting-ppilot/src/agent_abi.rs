//! pPilot client for the versioned pVisor Agent ABI.

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
    /// Discover the ABI from the current process environment.
    pub fn from_current_environment(
        client_id: impl Into<String>,
        role: AgentClientRole,
        agent_name: impl Into<String>,
    ) -> anyhow::Result<Option<Self>> {
        let environment = std::env::vars().collect::<BTreeMap<_, _>>();
        Self::from_environment(&environment, client_id, role, agent_name)
    }

    /// Discover a Unix Agent ABI endpoint from a pVisor-injected environment.
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
        let auth_token = environment
            .get(AGENT_ABI_TOKEN_ENV)
            .context("Agent ABI endpoint is present without an auth token")?
            .clone();
        Ok(Some(Self {
            endpoint: PathBuf::from(endpoint),
            auth_token,
            client_id: client_id.into(),
            role,
            agent_name: agent_name.into(),
        }))
    }
}

/// Low-frequency control client. Each call is one bounded JSON frame over a
/// fresh Unix connection, so reconnect does not lose established session state.
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
            body => unexpected_response("welcome", body),
        }
    }

    pub fn heartbeat(
        &mut self,
        lifecycle: AgentLifecycleState,
    ) -> anyhow::Result<AgentHeartbeatAck> {
        match self.request(AgentRequestBody::Heartbeat(lifecycle))? {
            AgentResponseBody::Heartbeat(ack) => Ok(ack),
            body => unexpected_response("heartbeat acknowledgement", body),
        }
    }

    pub fn register_process(
        &mut self,
        registration: AgentProcessRegistration,
    ) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::RegisterProcess(registration))?)
    }

    pub fn checkpoint_quiesced(&mut self, quiesced: AgentCheckpointQuiesced) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::CheckpointQuiesced(quiesced))?)
    }

    pub fn begin_effect(&mut self, begin: AgentEffectBegin) -> anyhow::Result<u64> {
        match self.request(AgentRequestBody::EffectBegin(begin))? {
            AgentResponseBody::EffectAccepted { sequence } => Ok(sequence),
            body => unexpected_response("effect acceptance", body),
        }
    }

    pub fn complete_effect(&mut self, completion: AgentEffectComplete) -> anyhow::Result<()> {
        expect_ack(self.request(AgentRequestBody::EffectComplete(completion))?)
    }

    fn request(&mut self, body: AgentRequestBody) -> anyhow::Result<AgentResponseBody> {
        let session_id = self
            .session_id
            .clone()
            .context("Agent ABI client is not connected")?;
        let request = AgentRequest::authenticated(session_id, body);
        Ok(exchange(&self.config.endpoint, &request)?.body)
    }
}

fn exchange(path: &Path, request: &AgentRequest) -> anyhow::Result<AgentResponse> {
    let mut stream = std::os::unix::net::UnixStream::connect(path)
        .with_context(|| format!("connect Agent ABI endpoint {}", path.display()))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    serde_json::to_writer(&mut stream, request).context("encode Agent ABI request")?;
    stream.write_all(b"\n")?;

    let mut frame = Vec::new();
    std::io::BufReader::new(stream)
        .by_ref()
        .take((AGENT_ABI_MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if frame.len() > AGENT_ABI_MAX_FRAME_BYTES {
        bail!("Agent ABI response exceeds {AGENT_ABI_MAX_FRAME_BYTES} bytes");
    }
    let response: AgentResponse =
        serde_json::from_slice(&frame).context("decode Agent ABI response")?;
    if let AgentResponseBody::Error { message } = &response.body {
        bail!("Agent ABI: {message}");
    }
    Ok(response)
}

fn expect_ack(body: AgentResponseBody) -> anyhow::Result<()> {
    match body {
        AgentResponseBody::Ack => Ok(()),
        body => unexpected_response("acknowledgement", body),
    }
}

fn unexpected_response<T>(expected: &str, body: AgentResponseBody) -> anyhow::Result<T> {
    bail!("expected Agent ABI {expected}, got {body:?}")
}

/// Convenience result for callers that only need to react to desired state.
pub fn checkpoint_directive(ack: &AgentHeartbeatAck) -> Option<(&str, u64)> {
    match &ack.directive {
        AgentDirective::Quiesce { checkpoint_id, .. } => {
            Some((checkpoint_id.as_str(), ack.directive_seq))
        }
        AgentDirective::Continue | AgentDirective::Shutdown { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_control::{AttemptId, RunId};
    use persisting_pvisor::{AgentAbiServer, AgentEffectOutcome};

    #[test]
    fn client_connects_and_drives_lifecycle() {
        let server =
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let config = AgentAbiClientConfig::from_environment(
            &server.environment(),
            "pilot-1",
            AgentClientRole::Pilot,
            "agent",
        )
        .unwrap()
        .unwrap();
        let mut client = AgentAbiClient::new(config);
        client.connect().unwrap();

        client
            .register_process(AgentProcessRegistration {
                pid: 7,
                role: "pilot".into(),
                executable: None,
            })
            .unwrap();
        let sequence = client
            .begin_effect(AgentEffectBegin {
                effect_id: "effect-1".into(),
                kind: "tool.call".into(),
                request_digest: "sha256:abc".into(),
                idempotency_key: None,
            })
            .unwrap();
        assert_eq!(sequence, 1);
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
        assert_eq!(snapshot.clients[0].client_id, "pilot-1");
        assert_eq!(snapshot.effects.len(), 1);
        assert_eq!(snapshot.processes.len(), 1);
    }
}
