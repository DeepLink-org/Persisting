//! Optional, cooperative Run-scoped AgentCtl channel owned by pVisor.

pub use persisting_agentctl::abi::{
    AgentCheckpointQuiesced, AgentClientRole, AgentDirective, AgentHeartbeatAck, AgentHello,
    AgentLifecycleState, AgentOperationBegin, AgentOperationComplete, AgentProcessRegistration,
    AgentRequest, AgentRequestBody, AgentResponse, AgentResponseBody, AgentWelcome,
    AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_VERSION, LEGACY_AGENT_ABI_ENDPOINT_ENV,
    LEGACY_AGENT_ABI_TOKEN_ENV, LEGACY_AGENT_ABI_TRANSPORT_ENV, LEGACY_AGENT_ABI_VERSION_ENV,
};
#[cfg(test)]
use persisting_agentctl::AgentOperationOutcome;
use persisting_agentctl::{AttemptId, RunId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub const AGENTCTL_MAX_SESSIONS: usize = 64;
pub const AGENTCTL_MAX_PROCESSES: usize = 1024;
pub const AGENTCTL_MAX_OPERATIONS: usize = 10_000;

const HEARTBEAT_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentClientSnapshot {
    pub client_id: String,
    pub agent_name: String,
    pub role: AgentClientRole,
    pub lifecycle: AgentLifecycleState,
    pub last_heartbeat_unix_ms: Option<u64>,
    #[serde(default)]
    pub stale: bool,
    pub quiesced_checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcessSnapshot {
    pub session_id: String,
    pub registration: AgentProcessRegistration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOperationSnapshot {
    pub session_id: String,
    pub sequence: u64,
    pub begin: AgentOperationBegin,
    pub completion: Option<AgentOperationComplete>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCtlSnapshot {
    pub run_id: String,
    pub attempt_id: String,
    pub directive_seq: u64,
    pub directive: AgentDirective,
    pub clients: Vec<AgentClientSnapshot>,
    pub processes: Vec<AgentProcessSnapshot>,
    #[serde(default, alias = "effects")]
    pub operations: Vec<AgentOperationSnapshot>,
}

#[derive(Debug)]
struct ClientSession {
    client_id: String,
    agent_name: String,
    role: AgentClientRole,
    lifecycle: AgentLifecycleState,
    last_seen_unix_ms: u64,
    last_heartbeat_unix_ms: Option<u64>,
    quiesced_checkpoint_id: Option<String>,
}

#[derive(Debug)]
struct AgentCtlState {
    run_id: String,
    attempt_id: String,
    auth_token: String,
    directive_seq: u64,
    directive: AgentDirective,
    sessions: HashMap<String, ClientSession>,
    processes: HashMap<(String, u32), AgentProcessRegistration>,
    effects: HashMap<String, AgentOperationSnapshot>,
    next_effect_sequence: u64,
}

impl AgentCtlState {
    fn new(run_id: &RunId, attempt_id: &AttemptId, auth_token: String) -> Self {
        Self {
            run_id: run_id.as_str().to_string(),
            attempt_id: attempt_id.as_str().to_string(),
            auth_token,
            directive_seq: 0,
            directive: AgentDirective::Continue,
            sessions: HashMap::new(),
            processes: HashMap::new(),
            effects: HashMap::new(),
            next_effect_sequence: 1,
        }
    }

    fn snapshot(&self) -> AgentCtlSnapshot {
        let now = crate::util::unix_now_ms();
        let mut clients = self
            .sessions
            .values()
            .map(|session| AgentClientSnapshot {
                client_id: session.client_id.clone(),
                agent_name: session.agent_name.clone(),
                role: session.role,
                lifecycle: session.lifecycle,
                last_heartbeat_unix_ms: session.last_heartbeat_unix_ms,
                stale: session_is_stale(session, now),
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

        AgentCtlSnapshot {
            run_id: self.run_id.clone(),
            attempt_id: self.attempt_id.clone(),
            directive_seq: self.directive_seq,
            directive: self.directive.clone(),
            clients,
            processes,
            operations: effects,
        }
    }
}

/// Cloneable pVisor-side control surface for one Run's cooperative AgentCtl channel.
#[derive(Clone)]
pub struct AgentCtlControl {
    endpoint: PathBuf,
    state: Arc<Mutex<AgentCtlState>>,
    delegated_snapshot: Arc<Mutex<Option<AgentCtlSnapshot>>>,
}

impl AgentCtlControl {
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub fn snapshot(&self) -> AgentCtlSnapshot {
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

    pub(crate) fn import_delegated_snapshot(&self, mut snapshot: AgentCtlSnapshot) {
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
pub struct AgentCtlServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    auth_token: String,
    control: AgentCtlControl,
}

impl AgentCtlServer {
    pub fn start(run_id: &RunId, attempt_id: &AttemptId) -> anyhow::Result<Self> {
        let socket_path = std::env::temp_dir().join(format!(
            "pvisor-agent-{}.sock",
            uuid::Uuid::new_v4().simple()
        ));
        let auth_token = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(Mutex::new(AgentCtlState::new(
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
        let thread_name = format!("pvisor-agentctl-{}", run_id.as_str());
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
        let control = AgentCtlControl {
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

    pub fn control(&self) -> AgentCtlControl {
        self.control.clone()
    }

    pub fn environment(&self) -> BTreeMap<String, String> {
        let endpoint = self.socket_path.display().to_string();
        let version = AGENTCTL_VERSION.to_string();
        BTreeMap::from([
            (
                persisting_agentctl::AGENTCTL_ENDPOINT_ENV.into(),
                endpoint.clone(),
            ),
            (
                persisting_agentctl::AGENTCTL_TOKEN_ENV.into(),
                self.auth_token.clone(),
            ),
            (
                persisting_agentctl::AGENTCTL_VERSION_ENV.into(),
                version.clone(),
            ),
            (
                persisting_agentctl::AGENTCTL_TRANSPORT_ENV.into(),
                "unix".into(),
            ),
            // Compatibility for clients that have not migrated to AgentCtl names.
            (LEGACY_AGENT_ABI_ENDPOINT_ENV.into(), endpoint),
            (LEGACY_AGENT_ABI_TOKEN_ENV.into(), self.auth_token.clone()),
            (LEGACY_AGENT_ABI_VERSION_ENV.into(), version),
            (LEGACY_AGENT_ABI_TRANSPORT_ENV.into(), "unix".into()),
        ])
    }
}

impl Drop for AgentCtlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn serve_connection(mut stream: std::os::unix::net::UnixStream, state: &Arc<Mutex<AgentCtlState>>) {
    // Accepted sockets inherit O_NONBLOCK from the listener on some Unix
    // platforms (including macOS); frames themselves use bounded blocking I/O.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = read_request(&stream)
        .map(|request| dispatch_request(request, state))
        .unwrap_or_else(|error| AgentResponse {
            body: AgentResponseBody::Error {
                message: error.to_string(),
            },
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
        .take((AGENTCTL_MAX_FRAME_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if frame.len() > AGENTCTL_MAX_FRAME_BYTES {
        anyhow::bail!("AgentCtl frame exceeds {AGENTCTL_MAX_FRAME_BYTES} bytes");
    }
    if frame.last() == Some(&b'\n') {
        frame.pop();
    }
    if frame.is_empty() {
        anyhow::bail!("empty AgentCtl frame");
    }
    Ok(serde_json::from_slice(&frame)?)
}

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn decode_agentctl_frame_for_fuzz(frame: &[u8]) -> anyhow::Result<AgentRequest> {
    anyhow::ensure!(
        !frame.is_empty() && frame.len() <= AGENTCTL_MAX_FRAME_BYTES,
        "invalid AgentCtl frame length"
    );
    let frame = frame.strip_suffix(b"\n").unwrap_or(frame);
    anyhow::ensure!(!frame.is_empty(), "empty AgentCtl frame");
    Ok(serde_json::from_slice(frame)?)
}

fn dispatch_request(request: AgentRequest, state: &Arc<Mutex<AgentCtlState>>) -> AgentResponse {
    let body = if request.version != AGENTCTL_VERSION {
        error_body(format!(
            "AgentCtl version mismatch: expected {AGENTCTL_VERSION}, got {}",
            request.version
        ))
    } else {
        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                return AgentResponse {
                    body: error_body("AgentCtl state lock poisoned"),
                };
            }
        };
        handle_body(request.session_id.as_deref(), request.body, &mut state)
            .unwrap_or_else(error_body)
    };
    AgentResponse { body }
}

fn handle_body(
    session_id: Option<&str>,
    body: AgentRequestBody,
    state: &mut AgentCtlState,
) -> Result<AgentResponseBody, String> {
    if let AgentRequestBody::Hello(hello) = body {
        if hello.auth_token != state.auth_token {
            return Err("invalid AgentCtl token".into());
        }
        if hello.client_id.trim().is_empty() || hello.agent_name.trim().is_empty() {
            return Err("client_id and agent_name must be non-empty".into());
        }
        let now = crate::util::unix_now_ms();
        let reclaim = state.sessions.iter().find_map(|(session_id, session)| {
            (session.client_id == hello.client_id && session_is_stale(session, now))
                .then(|| session_id.clone())
        });
        if let Some(session_id) = reclaim {
            let has_open_effects = state
                .effects
                .values()
                .any(|effect| effect.session_id == session_id && effect.completion.is_none());
            if has_open_effects {
                return Err(format!(
                    "stale client {} still owns declared open operations; refusing unsafe session replacement",
                    hello.client_id
                ));
            }
            state.sessions.remove(&session_id);
            state.processes.retain(|(owner, _), _| owner != &session_id);
        }
        if state
            .sessions
            .values()
            .any(|session| session.client_id == hello.client_id)
        {
            return Err(format!(
                "client {} already has a live session",
                hello.client_id
            ));
        }
        if state.sessions.len() >= AGENTCTL_MAX_SESSIONS {
            return Err(format!(
                "AgentCtl session limit of {AGENTCTL_MAX_SESSIONS} reached"
            ));
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        state.sessions.insert(
            session_id.clone(),
            ClientSession {
                client_id: hello.client_id,
                agent_name: hello.agent_name,
                role: hello.role,
                lifecycle: AgentLifecycleState::Starting,
                last_seen_unix_ms: now,
                last_heartbeat_unix_ms: None,
                quiesced_checkpoint_id: None,
            },
        );
        return Ok(AgentResponseBody::Welcome(AgentWelcome {
            session_id,
            heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
            directive_seq: state.directive_seq,
            directive: state.directive.clone(),
        }));
    }

    let session_id = session_id
        .ok_or_else(|| "authenticated AgentCtl request requires session_id".to_string())?;
    let now = crate::util::unix_now_ms();
    session_mut(state, session_id)?.last_seen_unix_ms = now;

    match body {
        AgentRequestBody::Hello(_) => unreachable!(),
        AgentRequestBody::Heartbeat(lifecycle) => {
            let session = session_mut(state, session_id)?;
            session.lifecycle = lifecycle;
            session.last_seen_unix_ms = now;
            session.last_heartbeat_unix_ms = Some(now);
            Ok(AgentResponseBody::Heartbeat(AgentHeartbeatAck {
                directive_seq: state.directive_seq,
                directive: state.directive.clone(),
            }))
        }
        AgentRequestBody::RegisterProcess(registration) => {
            if registration.pid == 0 || registration.role.trim().is_empty() {
                return Err("registered process requires a non-zero pid and non-empty role".into());
            }
            session_mut(state, session_id)?;
            if !state
                .processes
                .contains_key(&(session_id.to_string(), registration.pid))
                && state.processes.len() >= AGENTCTL_MAX_PROCESSES
            {
                return Err(format!(
                    "AgentCtl process registration limit of {AGENTCTL_MAX_PROCESSES} reached"
                ));
            }
            state
                .processes
                .insert((session_id.to_string(), registration.pid), registration);
            Ok(AgentResponseBody::Ack)
        }
        AgentRequestBody::CheckpointQuiesced(quiesced) => {
            record_quiesced(state, session_id, &quiesced)?;
            Ok(AgentResponseBody::Ack)
        }
        AgentRequestBody::EffectBegin(begin) => {
            session_mut(state, session_id)?;
            if begin.operation_id.trim().is_empty()
                || begin.kind.trim().is_empty()
                || begin.request_digest.trim().is_empty()
            {
                return Err("effect_id, kind, and request_digest must be non-empty".into());
            }
            if let Some(existing) = state.effects.get(&begin.operation_id) {
                if existing.session_id == session_id && existing.begin == begin {
                    return Ok(AgentResponseBody::OperationAccepted {
                        sequence: existing.sequence,
                    });
                }
                return Err(format!(
                    "effect {} already exists with different data",
                    begin.operation_id
                ));
            }
            if state.effects.len() >= AGENTCTL_MAX_OPERATIONS {
                return Err(format!(
                    "AgentCtl operation declaration limit of {AGENTCTL_MAX_OPERATIONS} reached"
                ));
            }
            let sequence = state.next_effect_sequence;
            state.next_effect_sequence = state.next_effect_sequence.saturating_add(1);
            state.effects.insert(
                begin.operation_id.clone(),
                AgentOperationSnapshot {
                    session_id: session_id.to_string(),
                    sequence,
                    begin,
                    completion: None,
                },
            );
            Ok(AgentResponseBody::OperationAccepted { sequence })
        }
        AgentRequestBody::EffectComplete(completion) => {
            session_mut(state, session_id)?;
            let effect = state
                .effects
                .get_mut(&completion.operation_id)
                .ok_or_else(|| format!("effect {} was not begun", completion.operation_id))?;
            if effect.session_id != session_id {
                return Err("operation declaration belongs to another AgentCtl session".into());
            }
            if let Some(existing) = &effect.completion {
                if existing != &completion {
                    return Err("effect was already completed with a different outcome".into());
                }
            } else {
                effect.completion = Some(completion);
            }
            Ok(AgentResponseBody::Ack)
        }
    }
}

fn session_is_stale(session: &ClientSession, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(session.last_seen_unix_ms) > HEARTBEAT_INTERVAL_MS * 3
}

fn session_mut<'a>(
    state: &'a mut AgentCtlState,
    session_id: &str,
) -> Result<&'a mut ClientSession, String> {
    state
        .sessions
        .get_mut(session_id)
        .ok_or_else(|| "unknown AgentCtl session".to_string())
}

fn record_quiesced(
    state: &mut AgentCtlState,
    session_id: &str,
    quiesced: &AgentCheckpointQuiesced,
) -> Result<(), String> {
    let (checkpoint_id, directive_seq) = match &state.directive {
        AgentDirective::Quiesce { checkpoint_id, .. } => {
            (checkpoint_id.clone(), state.directive_seq)
        }
        _ => {
            return Err("pVisor has not requested quiescence".into());
        }
    };
    if quiesced.directive_seq != directive_seq || quiesced.checkpoint_id != checkpoint_id {
        return Err("quiesced acknowledgement does not match the active directive".into());
    }
    let has_open_effects = state
        .effects
        .values()
        .any(|effect| effect.session_id == session_id && effect.completion.is_none());
    if has_open_effects {
        return Err("AgentCtl session still has declared open operations".into());
    }
    let session = session_mut(state, session_id)?;
    session.lifecycle = AgentLifecycleState::Quiesced;
    session.quiesced_checkpoint_id = Some(checkpoint_id);
    Ok(())
}

fn lock_state(state: &Arc<Mutex<AgentCtlState>>) -> std::sync::MutexGuard<'_, AgentCtlState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn error_body(message: impl Into<String>) -> AgentResponseBody {
    AgentResponseBody::Error {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn connect(server: &AgentCtlServer) -> String {
        let request = AgentRequest::hello(AgentHello {
            auth_token: server.auth_token.clone(),
            client_id: "pilot-1".into(),
            role: AgentClientRole::Pilot,
            agent_name: "agent".into(),
        });
        match exchange(&server.socket_path, &request).body {
            AgentResponseBody::Welcome(welcome) => welcome.session_id,
            body => panic!("unexpected response: {body:?}"),
        }
    }

    #[test]
    fn rejects_invalid_token_and_protocol_version() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let unauthorized = exchange(
            &server.socket_path,
            &AgentRequest::hello(AgentHello {
                auth_token: "wrong".into(),
                client_id: "pilot".into(),
                role: AgentClientRole::Pilot,
                agent_name: "agent".into(),
            }),
        );
        assert!(
            matches!(
                &unauthorized.body,
                AgentResponseBody::Error { message } if message.contains("token")
            ),
            "unexpected response: {:?}",
            unauthorized.body
        );

        let mut wrong_version = AgentRequest::hello(AgentHello {
            auth_token: server.auth_token.clone(),
            client_id: "pilot".into(),
            role: AgentClientRole::Pilot,
            agent_name: "agent".into(),
        });
        wrong_version.version += 1;
        assert!(matches!(
            exchange(&server.socket_path, &wrong_version).body,
            AgentResponseBody::Error { message } if message.contains("version mismatch")
        ));
    }

    #[test]
    fn duplicate_live_client_is_rejected_and_stale_state_is_reported() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server);
        let duplicate = AgentRequest::hello(AgentHello {
            auth_token: server.auth_token.clone(),
            client_id: "pilot-1".into(),
            role: AgentClientRole::Pilot,
            agent_name: "agent".into(),
        });
        assert!(matches!(
            exchange(&server.socket_path, &duplicate).body,
            AgentResponseBody::Error { message } if message.contains("live session")
        ));

        {
            let mut state = lock_state(&server.control.state);
            state
                .sessions
                .get_mut(&session_id)
                .unwrap()
                .last_seen_unix_ms =
                crate::util::unix_now_ms().saturating_sub(HEARTBEAT_INTERVAL_MS * 4);
        }
        assert!(server.control.snapshot().clients[0].stale);
    }

    #[test]
    fn heartbeat_quiesce_process_and_effect_lifecycle() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server);
        let directive_seq = server.control.request_quiesce("checkpoint-1", None);

        let heartbeat = exchange(
            &server.socket_path,
            &AgentRequest::authenticated(
                &session_id,
                AgentRequestBody::Heartbeat(AgentLifecycleState::Quiescing),
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
            &session_id,
            AgentRequestBody::RegisterProcess(AgentProcessRegistration {
                pid: 42,
                role: "worker".into(),
                executable: Some("python".into()),
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &register).body,
            AgentResponseBody::Ack
        ));

        let begin = AgentOperationBegin {
            operation_id: "effect-1".into(),
            kind: "tool.call".into(),
            request_digest: "sha256:abc".into(),
            idempotency_key: Some("idem-1".into()),
        };
        let begin_response = exchange(
            &server.socket_path,
            &AgentRequest::authenticated(&session_id, AgentRequestBody::EffectBegin(begin)),
        );
        assert!(matches!(
            begin_response.body,
            AgentResponseBody::OperationAccepted { sequence: 1 }
        ));
        let complete = AgentRequest::authenticated(
            &session_id,
            AgentRequestBody::EffectComplete(AgentOperationComplete {
                operation_id: "effect-1".into(),
                outcome: AgentOperationOutcome::Committed,
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &complete).body,
            AgentResponseBody::Ack
        ));

        let quiesced = AgentRequest::authenticated(
            &session_id,
            AgentRequestBody::CheckpointQuiesced(AgentCheckpointQuiesced {
                checkpoint_id: "checkpoint-1".into(),
                directive_seq,
            }),
        );
        assert!(matches!(
            exchange(&server.socket_path, &quiesced).body,
            AgentResponseBody::Ack
        ));

        let snapshot = server.control.snapshot();
        assert_eq!(
            snapshot.clients[0].quiesced_checkpoint_id.as_deref(),
            Some("checkpoint-1")
        );
        assert_eq!(snapshot.processes[0].registration.pid, 42);
        assert_eq!(
            snapshot.operations[0].completion.as_ref().unwrap().outcome,
            AgentOperationOutcome::Committed
        );
    }

    #[test]
    fn delegated_snapshot_replaces_empty_host_transport_observation() {
        let server =
            AgentCtlServer::start(&RunId::new("outer-run"), &AttemptId::new("outer-attempt"))
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
