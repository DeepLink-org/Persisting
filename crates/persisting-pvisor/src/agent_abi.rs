//! Run-scoped Unix transport and state machine for the pPilot ↔ pVisor Agent ABI.

use persisting_proto::{
    AgentAbiError, AgentAbiErrorCode, AgentAck, AgentCapability, AgentCheckpointQuiesced,
    AgentClientRole, AgentDirective, AgentEffectAccepted, AgentEffectBegin, AgentEffectComplete,
    AgentHeartbeatAck, AgentLifecycleState, AgentProcessRegistration, AgentRequest,
    AgentRequestBody, AgentResponse, AgentResponseBody, AgentWelcome, AttemptId, RunId,
    AGENT_ABI_MAX_FRAME_BYTES, AGENT_ABI_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub const AGENT_ABI_ENDPOINT_ENV: &str = "PERSISTING_AGENT_ABI_ENDPOINT";
pub const AGENT_ABI_TOKEN_ENV: &str = "PERSISTING_AGENT_ABI_TOKEN";
pub const AGENT_ABI_VERSION_ENV: &str = "PERSISTING_AGENT_ABI_VERSION";
pub const AGENT_ABI_TRANSPORT_ENV: &str = "PERSISTING_AGENT_ABI_TRANSPORT";

const HEARTBEAT_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentClientSnapshot {
    pub client_id: String,
    pub agent_name: String,
    pub role: AgentClientRole,
    pub capabilities: Vec<AgentCapability>,
    pub lifecycle: AgentLifecycleState,
    pub last_heartbeat_unix_ms: Option<u64>,
    pub quiesced_checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcessSnapshot {
    pub session_id: String,
    pub registration: AgentProcessRegistration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEffectSnapshot {
    pub session_id: String,
    pub sequence: u64,
    pub begin: AgentEffectBegin,
    pub completion: Option<AgentEffectComplete>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAbiSnapshot {
    pub run_id: String,
    pub attempt_id: String,
    pub directive_seq: u64,
    pub directive: AgentDirective,
    pub clients: Vec<AgentClientSnapshot>,
    pub processes: Vec<AgentProcessSnapshot>,
    pub effects: Vec<AgentEffectSnapshot>,
}

#[derive(Debug)]
struct ClientSession {
    client_id: String,
    agent_name: String,
    role: AgentClientRole,
    capabilities: BTreeSet<AgentCapability>,
    lifecycle: AgentLifecycleState,
    last_heartbeat_unix_ms: Option<u64>,
    quiesced_checkpoint_id: Option<String>,
}

#[derive(Debug)]
struct AgentAbiState {
    run_id: String,
    attempt_id: String,
    auth_token: String,
    supported_capabilities: BTreeSet<AgentCapability>,
    directive_seq: u64,
    directive: AgentDirective,
    sessions: HashMap<String, ClientSession>,
    processes: HashMap<(String, u32), AgentProcessRegistration>,
    effects: HashMap<String, AgentEffectSnapshot>,
    next_effect_sequence: u64,
}

impl AgentAbiState {
    fn new(run_id: &RunId, attempt_id: &AttemptId, auth_token: String) -> Self {
        Self {
            run_id: run_id.as_str().to_string(),
            attempt_id: attempt_id.as_str().to_string(),
            auth_token,
            supported_capabilities: [
                AgentCapability::Heartbeat,
                AgentCapability::ProcessRegistry,
                AgentCapability::CheckpointQuiesce,
                AgentCapability::EffectJournal,
            ]
            .into_iter()
            .collect(),
            directive_seq: 0,
            directive: AgentDirective::Continue,
            sessions: HashMap::new(),
            processes: HashMap::new(),
            effects: HashMap::new(),
            next_effect_sequence: 1,
        }
    }

    fn snapshot(&self) -> AgentAbiSnapshot {
        let mut clients = self
            .sessions
            .values()
            .map(|session| AgentClientSnapshot {
                client_id: session.client_id.clone(),
                agent_name: session.agent_name.clone(),
                role: session.role,
                capabilities: session.capabilities.iter().copied().collect(),
                lifecycle: session.lifecycle,
                last_heartbeat_unix_ms: session.last_heartbeat_unix_ms,
                quiesced_checkpoint_id: session.quiesced_checkpoint_id.clone(),
            })
            .collect::<Vec<_>>();
        clients.sort_by(|left, right| left.client_id.cmp(&right.client_id));

        let mut processes = self
            .processes
            .iter()
            .map(|((session_id, _), registration)| AgentProcessSnapshot {
                session_id: session_id.clone(),
                registration: registration.clone(),
            })
            .collect::<Vec<_>>();
        processes.sort_by_key(|process| process.registration.pid);

        let mut effects = self.effects.values().cloned().collect::<Vec<_>>();
        effects.sort_by_key(|effect| effect.sequence);

        AgentAbiSnapshot {
            run_id: self.run_id.clone(),
            attempt_id: self.attempt_id.clone(),
            directive_seq: self.directive_seq,
            directive: self.directive.clone(),
            clients,
            processes,
            effects,
        }
    }
}

/// Cloneable pVisor-side control surface for one Run's Agent ABI.
#[derive(Clone)]
pub struct AgentAbiControl {
    endpoint: PathBuf,
    state: Arc<Mutex<AgentAbiState>>,
    delegated_snapshot: Arc<Mutex<Option<AgentAbiSnapshot>>>,
}

impl AgentAbiControl {
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub fn snapshot(&self) -> AgentAbiSnapshot {
        if let Some(snapshot) = self
            .delegated_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return snapshot;
        }
        lock_state(&self.state).snapshot()
    }

    pub(crate) fn import_delegated_snapshot(&self, mut snapshot: AgentAbiSnapshot) {
        let state = lock_state(&self.state);
        snapshot.run_id = state.run_id.clone();
        snapshot.attempt_id = state.attempt_id.clone();
        drop(state);
        *self
            .delegated_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snapshot);
    }

    pub fn request_quiesce(
        &self,
        checkpoint_id: impl Into<String>,
        deadline_unix_ms: Option<u64>,
    ) -> u64 {
        self.set_directive(AgentDirective::Quiesce {
            checkpoint_id: checkpoint_id.into(),
            deadline_unix_ms,
        })
    }

    pub fn continue_execution(&self) -> u64 {
        self.set_directive(AgentDirective::Continue)
    }

    pub fn request_shutdown(&self, reason: Option<String>) -> u64 {
        self.set_directive(AgentDirective::Shutdown { reason })
    }

    fn set_directive(&self, directive: AgentDirective) -> u64 {
        let mut state = lock_state(&self.state);
        state.directive_seq = state.directive_seq.saturating_add(1);
        state.directive = directive;
        for session in state.sessions.values_mut() {
            session.quiesced_checkpoint_id = None;
        }
        state.directive_seq
    }
}

/// Owns the Run-scoped Unix listener. Dropping it closes and removes the endpoint.
pub struct AgentAbiServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    auth_token: String,
    control: AgentAbiControl,
}

impl AgentAbiServer {
    pub fn start(run_id: &RunId, attempt_id: &AttemptId) -> anyhow::Result<Self> {
        let socket_path = std::env::temp_dir().join(format!(
            "pvisor-agent-{}.sock",
            uuid::Uuid::new_v4().simple()
        ));
        let auth_token = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(Mutex::new(AgentAbiState::new(
            run_id,
            attempt_id,
            auth_token.clone(),
        )));
        let listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
        if let Err(error) = fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600)) {
            let _ = fs::remove_file(&socket_path);
            return Err(error.into());
        }
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = fs::remove_file(&socket_path);
            return Err(error.into());
        }

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_state = Arc::clone(&state);
        let thread_name = format!("pvisor-agent-abi-{}", run_id.as_str());
        let join = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => serve_connection(stream, &thread_state),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
        let join = match join {
            Ok(join) => join,
            Err(error) => {
                let _ = fs::remove_file(&socket_path);
                return Err(error.into());
            }
        };
        let control = AgentAbiControl {
            endpoint: socket_path.clone(),
            state,
            delegated_snapshot: Arc::new(Mutex::new(None)),
        };
        Ok(Self {
            stop,
            join: Some(join),
            socket_path,
            auth_token,
            control,
        })
    }

    pub fn control(&self) -> AgentAbiControl {
        self.control.clone()
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                AGENT_ABI_ENDPOINT_ENV.into(),
                self.socket_path.display().to_string(),
            ),
            (AGENT_ABI_TOKEN_ENV.into(), self.auth_token.clone()),
            (AGENT_ABI_VERSION_ENV.into(), AGENT_ABI_VERSION.to_string()),
            (AGENT_ABI_TRANSPORT_ENV.into(), "unix".into()),
        ])
    }
}

impl Drop for AgentAbiServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn serve_connection(mut stream: std::os::unix::net::UnixStream, state: &Arc<Mutex<AgentAbiState>>) {
    // Accepted sockets inherit O_NONBLOCK from the listener on some Unix
    // platforms (including macOS); frames themselves use bounded blocking I/O.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = read_request(&stream)
        .map(|request| dispatch_request(request, state))
        .unwrap_or_else(|error| AgentResponse {
            version: AGENT_ABI_VERSION,
            request_id: String::new(),
            body: AgentResponseBody::Error(AgentAbiError {
                code: AgentAbiErrorCode::MalformedRequest,
                message: error.to_string(),
            }),
        });
    if let Ok(mut body) = serde_json::to_vec(&response) {
        body.push(b'\n');
        let _ = stream.write_all(&body);
    }
}

fn read_request(stream: &std::os::unix::net::UnixStream) -> anyhow::Result<AgentRequest> {
    let mut frame = Vec::new();
    let mut reader = std::io::BufReader::new(stream);
    reader
        .by_ref()
        .take((AGENT_ABI_MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if frame.len() > AGENT_ABI_MAX_FRAME_BYTES {
        anyhow::bail!("Agent ABI frame exceeds {AGENT_ABI_MAX_FRAME_BYTES} bytes");
    }
    if frame.last() == Some(&b'\n') {
        frame.pop();
    }
    if frame.is_empty() {
        anyhow::bail!("empty Agent ABI frame");
    }
    Ok(serde_json::from_slice(&frame)?)
}

fn dispatch_request(request: AgentRequest, state: &Arc<Mutex<AgentAbiState>>) -> AgentResponse {
    let request_id = request.request_id.clone();
    let body = if request.version != AGENT_ABI_VERSION {
        error_body(
            AgentAbiErrorCode::VersionMismatch,
            format!(
                "Agent ABI version mismatch: expected {AGENT_ABI_VERSION}, got {}",
                request.version
            ),
        )
    } else {
        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                return AgentResponse {
                    version: AGENT_ABI_VERSION,
                    request_id,
                    body: error_body(AgentAbiErrorCode::Internal, "Agent ABI state lock poisoned"),
                };
            }
        };
        handle_body(request.session_id.as_deref(), request.body, &mut state)
            .unwrap_or_else(|error| error_body(error.code, error.message))
    };
    AgentResponse {
        version: AGENT_ABI_VERSION,
        request_id,
        body,
    }
}

fn handle_body(
    session_id: Option<&str>,
    body: AgentRequestBody,
    state: &mut AgentAbiState,
) -> Result<AgentResponseBody, AgentAbiError> {
    if let AgentRequestBody::Hello(hello) = body {
        if hello.auth_token != state.auth_token {
            return Err(abi_error(
                AgentAbiErrorCode::Unauthorized,
                "invalid Agent ABI token",
            ));
        }
        if hello.client_id.trim().is_empty() || hello.agent_name.trim().is_empty() {
            return Err(abi_error(
                AgentAbiErrorCode::InvalidTransition,
                "client_id and agent_name must be non-empty",
            ));
        }
        let capabilities = hello
            .capabilities
            .into_iter()
            .filter(|capability| state.supported_capabilities.contains(capability))
            .collect::<BTreeSet<_>>();
        let session_id = uuid::Uuid::new_v4().to_string();
        state.sessions.insert(
            session_id.clone(),
            ClientSession {
                client_id: hello.client_id,
                agent_name: hello.agent_name,
                role: hello.role,
                capabilities: capabilities.clone(),
                lifecycle: AgentLifecycleState::Starting,
                last_heartbeat_unix_ms: None,
                quiesced_checkpoint_id: None,
            },
        );
        let checkpoint_capable = capabilities.contains(&AgentCapability::CheckpointQuiesce);
        return Ok(AgentResponseBody::Welcome(AgentWelcome {
            session_id,
            run_id: state.run_id.clone(),
            attempt_id: state.attempt_id.clone(),
            accepted_capabilities: capabilities.into_iter().collect(),
            heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
            directive_seq: state.directive_seq,
            directive: directive_for(checkpoint_capable, state),
        }));
    }

    let session_id = session_id.ok_or_else(|| {
        abi_error(
            AgentAbiErrorCode::InvalidSession,
            "authenticated Agent ABI request requires session_id",
        )
    })?;
    let now = crate::util::unix_now_ms();

    match body {
        AgentRequestBody::Hello(_) => unreachable!(),
        AgentRequestBody::Heartbeat(heartbeat) => {
            let checkpoint_capable = {
                let session = session_mut(state, session_id, AgentCapability::Heartbeat)?;
                session.lifecycle = heartbeat.state;
                session.last_heartbeat_unix_ms = Some(now);
                session
                    .capabilities
                    .contains(&AgentCapability::CheckpointQuiesce)
            };
            Ok(AgentResponseBody::Heartbeat(AgentHeartbeatAck {
                server_time_unix_ms: now,
                directive_seq: state.directive_seq,
                directive: directive_for(checkpoint_capable, state),
            }))
        }
        AgentRequestBody::RegisterProcess(registration) => {
            if registration.pid == 0 || registration.role.trim().is_empty() {
                return Err(abi_error(
                    AgentAbiErrorCode::InvalidTransition,
                    "registered process requires a non-zero pid and non-empty role",
                ));
            }
            session_mut(state, session_id, AgentCapability::ProcessRegistry)?;
            state
                .processes
                .insert((session_id.to_string(), registration.pid), registration);
            Ok(AgentResponseBody::Ack(AgentAck {
                accepted_at_unix_ms: now,
            }))
        }
        AgentRequestBody::CheckpointQuiesced(quiesced) => {
            record_quiesced(state, session_id, &quiesced)?;
            Ok(AgentResponseBody::Ack(AgentAck {
                accepted_at_unix_ms: now,
            }))
        }
        AgentRequestBody::EffectBegin(begin) => {
            session_mut(state, session_id, AgentCapability::EffectJournal)?;
            if begin.effect_id.trim().is_empty()
                || begin.kind.trim().is_empty()
                || begin.request_digest.trim().is_empty()
            {
                return Err(abi_error(
                    AgentAbiErrorCode::InvalidTransition,
                    "effect_id, kind, and request_digest must be non-empty",
                ));
            }
            if let Some(existing) = state.effects.get(&begin.effect_id) {
                if existing.session_id == session_id && existing.begin == begin {
                    return Ok(AgentResponseBody::EffectAccepted(AgentEffectAccepted {
                        sequence: existing.sequence,
                        accepted_at_unix_ms: now,
                    }));
                }
                return Err(abi_error(
                    AgentAbiErrorCode::InvalidTransition,
                    format!(
                        "effect {} already exists with different data",
                        begin.effect_id
                    ),
                ));
            }
            let sequence = state.next_effect_sequence;
            state.next_effect_sequence = state.next_effect_sequence.saturating_add(1);
            state.effects.insert(
                begin.effect_id.clone(),
                AgentEffectSnapshot {
                    session_id: session_id.to_string(),
                    sequence,
                    begin,
                    completion: None,
                },
            );
            Ok(AgentResponseBody::EffectAccepted(AgentEffectAccepted {
                sequence,
                accepted_at_unix_ms: now,
            }))
        }
        AgentRequestBody::EffectComplete(completion) => {
            session_mut(state, session_id, AgentCapability::EffectJournal)?;
            let effect = state
                .effects
                .get_mut(&completion.effect_id)
                .ok_or_else(|| {
                    abi_error(
                        AgentAbiErrorCode::InvalidTransition,
                        format!("effect {} was not begun", completion.effect_id),
                    )
                })?;
            if effect.session_id != session_id {
                return Err(abi_error(
                    AgentAbiErrorCode::InvalidTransition,
                    "effect belongs to another Agent ABI session",
                ));
            }
            if let Some(existing) = &effect.completion {
                if existing != &completion {
                    return Err(abi_error(
                        AgentAbiErrorCode::InvalidTransition,
                        "effect was already completed with a different outcome",
                    ));
                }
            } else {
                effect.completion = Some(completion);
            }
            Ok(AgentResponseBody::Ack(AgentAck {
                accepted_at_unix_ms: now,
            }))
        }
    }
}

fn session_mut<'a>(
    state: &'a mut AgentAbiState,
    session_id: &str,
    capability: AgentCapability,
) -> Result<&'a mut ClientSession, AgentAbiError> {
    let session = state.sessions.get_mut(session_id).ok_or_else(|| {
        abi_error(
            AgentAbiErrorCode::InvalidSession,
            "unknown Agent ABI session",
        )
    })?;
    if !session.capabilities.contains(&capability) {
        return Err(abi_error(
            AgentAbiErrorCode::CapabilityNotNegotiated,
            format!("Agent ABI capability {capability:?} was not negotiated"),
        ));
    }
    Ok(session)
}

fn record_quiesced(
    state: &mut AgentAbiState,
    session_id: &str,
    quiesced: &AgentCheckpointQuiesced,
) -> Result<(), AgentAbiError> {
    let (checkpoint_id, directive_seq) = match &state.directive {
        AgentDirective::Quiesce { checkpoint_id, .. } => {
            (checkpoint_id.clone(), state.directive_seq)
        }
        _ => {
            return Err(abi_error(
                AgentAbiErrorCode::InvalidTransition,
                "pVisor has not requested quiescence",
            ));
        }
    };
    if quiesced.directive_seq != directive_seq || quiesced.checkpoint_id != checkpoint_id {
        return Err(abi_error(
            AgentAbiErrorCode::InvalidTransition,
            "quiesced acknowledgement does not match the active directive",
        ));
    }
    let reported_open = quiesced
        .open_effect_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let recorded_open = state
        .effects
        .values()
        .filter(|effect| effect.session_id == session_id && effect.completion.is_none())
        .map(|effect| effect.begin.effect_id.clone())
        .collect::<BTreeSet<_>>();
    if reported_open != recorded_open {
        return Err(abi_error(
            AgentAbiErrorCode::InvalidTransition,
            "reported open effects do not match pVisor's effect journal",
        ));
    }
    if quiesced
        .last_effect_seq
        .is_some_and(|sequence| sequence >= state.next_effect_sequence)
    {
        return Err(abi_error(
            AgentAbiErrorCode::InvalidTransition,
            "last_effect_seq is ahead of pVisor's effect journal",
        ));
    }
    let session = session_mut(state, session_id, AgentCapability::CheckpointQuiesce)?;
    session.lifecycle = AgentLifecycleState::Quiesced;
    session.quiesced_checkpoint_id = Some(checkpoint_id);
    Ok(())
}

fn directive_for(checkpoint_capable: bool, state: &AgentAbiState) -> AgentDirective {
    match &state.directive {
        AgentDirective::Quiesce { .. } if !checkpoint_capable => AgentDirective::Continue,
        directive => directive.clone(),
    }
}

fn lock_state(state: &Arc<Mutex<AgentAbiState>>) -> std::sync::MutexGuard<'_, AgentAbiState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn abi_error(code: AgentAbiErrorCode, message: impl Into<String>) -> AgentAbiError {
    AgentAbiError {
        code,
        message: message.into(),
    }
}

fn error_body(code: AgentAbiErrorCode, message: impl Into<String>) -> AgentResponseBody {
    AgentResponseBody::Error(abi_error(code, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_proto::{
        AgentClientRole, AgentEffectComplete, AgentEffectOutcome, AgentHeartbeat, AgentHello,
        AgentRequest,
    };

    fn exchange(path: &Path, request: &AgentRequest) -> AgentResponse {
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        serde_json::to_writer(&mut stream, request).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn connect(server: &AgentAbiServer) -> String {
        let request = AgentRequest::hello(
            "hello-1",
            AgentHello {
                auth_token: server.auth_token.clone(),
                client_id: "pilot-1".into(),
                role: AgentClientRole::Pilot,
                agent_name: "agent".into(),
                capabilities: vec![
                    AgentCapability::Heartbeat,
                    AgentCapability::ProcessRegistry,
                    AgentCapability::CheckpointQuiesce,
                    AgentCapability::EffectJournal,
                ],
            },
        );
        match exchange(&server.socket_path, &request).body {
            AgentResponseBody::Welcome(welcome) => welcome.session_id,
            body => panic!("unexpected response: {body:?}"),
        }
    }

    #[test]
    fn rejects_invalid_token_and_protocol_version() {
        let server =
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let unauthorized = exchange(
            &server.socket_path,
            &AgentRequest::hello(
                "bad-token",
                AgentHello {
                    auth_token: "wrong".into(),
                    client_id: "pilot".into(),
                    role: AgentClientRole::Pilot,
                    agent_name: "agent".into(),
                    capabilities: vec![],
                },
            ),
        );
        assert!(
            matches!(
                &unauthorized.body,
                AgentResponseBody::Error(AgentAbiError {
                    code: AgentAbiErrorCode::Unauthorized,
                    ..
                })
            ),
            "unexpected response: {:?}",
            unauthorized.body
        );

        let mut wrong_version = AgentRequest::hello(
            "bad-version",
            AgentHello {
                auth_token: server.auth_token.clone(),
                client_id: "pilot".into(),
                role: AgentClientRole::Pilot,
                agent_name: "agent".into(),
                capabilities: vec![],
            },
        );
        wrong_version.version += 1;
        assert!(matches!(
            exchange(&server.socket_path, &wrong_version).body,
            AgentResponseBody::Error(AgentAbiError {
                code: AgentAbiErrorCode::VersionMismatch,
                ..
            })
        ));
    }

    #[test]
    fn heartbeat_quiesce_process_and_effect_lifecycle() {
        let server =
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server);
        let directive_seq = server.control.request_quiesce("checkpoint-1", None);

        let heartbeat = exchange(
            &server.socket_path,
            &AgentRequest::authenticated(
                "heartbeat-1",
                &session_id,
                AgentRequestBody::Heartbeat(AgentHeartbeat {
                    state: AgentLifecycleState::Quiescing,
                    message: None,
                }),
            ),
        );
        assert!(matches!(
            heartbeat.body,
            AgentResponseBody::Heartbeat(AgentHeartbeatAck {
                directive: AgentDirective::Quiesce { .. },
                ..
            })
        ));

        let register = AgentRequest::authenticated(
            "process-1",
            &session_id,
            AgentRequestBody::RegisterProcess(AgentProcessRegistration {
                pid: 42,
                parent_pid: Some(1),
                role: "worker".into(),
                executable: Some("python".into()),
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &register).body,
            AgentResponseBody::Ack(_)
        ));

        let begin = AgentEffectBegin {
            effect_id: "effect-1".into(),
            kind: "tool.call".into(),
            request_digest: "sha256:abc".into(),
            idempotency_key: Some("idem-1".into()),
        };
        let begin_response = exchange(
            &server.socket_path,
            &AgentRequest::authenticated(
                "effect-begin",
                &session_id,
                AgentRequestBody::EffectBegin(begin),
            ),
        );
        assert!(matches!(
            begin_response.body,
            AgentResponseBody::EffectAccepted(AgentEffectAccepted { sequence: 1, .. })
        ));
        let complete = AgentRequest::authenticated(
            "effect-complete",
            &session_id,
            AgentRequestBody::EffectComplete(AgentEffectComplete {
                effect_id: "effect-1".into(),
                outcome: AgentEffectOutcome::Committed,
                response_ref: Some("chronicle://effect-1".into()),
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &complete).body,
            AgentResponseBody::Ack(_)
        ));

        let quiesced = AgentRequest::authenticated(
            "quiesced-1",
            &session_id,
            AgentRequestBody::CheckpointQuiesced(AgentCheckpointQuiesced {
                checkpoint_id: "checkpoint-1".into(),
                directive_seq,
                last_effect_seq: Some(1),
                open_effect_ids: Vec::new(),
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &quiesced).body,
            AgentResponseBody::Ack(_)
        ));

        let snapshot = server.control.snapshot();
        assert_eq!(
            snapshot.clients[0].quiesced_checkpoint_id.as_deref(),
            Some("checkpoint-1")
        );
        assert_eq!(snapshot.processes[0].registration.pid, 42);
        assert_eq!(
            snapshot.effects[0].completion.as_ref().unwrap().outcome,
            AgentEffectOutcome::Committed
        );
    }

    #[test]
    fn delegated_snapshot_replaces_empty_host_transport_observation() {
        let server =
            AgentAbiServer::start(&RunId::new("outer-run"), &AttemptId::new("outer-attempt"))
                .unwrap();
        let mut delegated = server.control.snapshot();
        delegated.run_id = "inner-run".into();
        delegated.attempt_id = "inner-attempt".into();
        delegated.directive_seq = 7;
        server.control.import_delegated_snapshot(delegated);
        let imported = server.control.snapshot();
        assert_eq!(imported.run_id, "outer-run");
        assert_eq!(imported.attempt_id, "outer-attempt");
        assert_eq!(imported.directive_seq, 7);
    }
}
