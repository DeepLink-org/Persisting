//! Run-scoped Unix control channel owned by pVisor.

use persisting_control::{AttemptId, RunId};
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

pub const AGENT_ABI_VERSION: u32 = 2;
pub const AGENT_ABI_MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const AGENT_ABI_MAX_SESSIONS: usize = 64;
pub const AGENT_ABI_MAX_PROCESSES: usize = 1024;
pub const AGENT_ABI_MAX_EFFECTS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentClientRole {
    Pilot,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleState {
    Starting,
    Running,
    Idle,
    Quiescing,
    Quiesced,
    Stopping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDirective {
    Continue,
    Quiesce {
        checkpoint_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_unix_ms: Option<u64>,
    },
    Shutdown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHello {
    pub auth_token: String,
    pub client_id: String,
    pub role: AgentClientRole,
    pub agent_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProcessRegistration {
    pub pid: u32,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCheckpointQuiesced {
    pub checkpoint_id: String,
    pub directive_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEffectBegin {
    pub effect_id: String,
    pub kind: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEffectOutcome {
    Committed,
    Aborted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEffectComplete {
    pub effect_id: String,
    pub outcome: AgentEffectOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentRequestBody {
    Hello(AgentHello),
    Heartbeat(AgentLifecycleState),
    RegisterProcess(AgentProcessRegistration),
    CheckpointQuiesced(AgentCheckpointQuiesced),
    EffectBegin(AgentEffectBegin),
    EffectComplete(AgentEffectComplete),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRequest {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub body: AgentRequestBody,
}

impl AgentRequest {
    pub fn hello(hello: AgentHello) -> Self {
        Self {
            version: AGENT_ABI_VERSION,
            session_id: None,
            body: AgentRequestBody::Hello(hello),
        }
    }

    pub fn authenticated(session_id: impl Into<String>, body: AgentRequestBody) -> Self {
        Self {
            version: AGENT_ABI_VERSION,
            session_id: Some(session_id.into()),
            body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWelcome {
    pub session_id: String,
    pub heartbeat_interval_ms: u64,
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeatAck {
    pub directive_seq: u64,
    pub directive: AgentDirective,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum AgentResponseBody {
    Welcome(AgentWelcome),
    Heartbeat(AgentHeartbeatAck),
    Ack,
    EffectAccepted { sequence: u64 },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResponse {
    pub body: AgentResponseBody,
}

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
    lifecycle: AgentLifecycleState,
    last_seen_unix_ms: u64,
    last_heartbeat_unix_ms: Option<u64>,
    quiesced_checkpoint_id: Option<String>,
}

#[derive(Debug)]
struct AgentAbiState {
    run_id: String,
    attempt_id: String,
    auth_token: String,
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
            directive_seq: 0,
            directive: AgentDirective::Continue,
            sessions: HashMap::new(),
            processes: HashMap::new(),
            effects: HashMap::new(),
            next_effect_sequence: 1,
        }
    }

    fn snapshot(&self) -> AgentAbiSnapshot {
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

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub fn decode_agent_abi_frame_for_fuzz(frame: &[u8]) -> anyhow::Result<AgentRequest> {
    anyhow::ensure!(
        !frame.is_empty() && frame.len() <= AGENT_ABI_MAX_FRAME_BYTES,
        "invalid Agent ABI frame length"
    );
    let frame = frame.strip_suffix(b"\n").unwrap_or(frame);
    anyhow::ensure!(!frame.is_empty(), "empty Agent ABI frame");
    Ok(serde_json::from_slice(frame)?)
}

fn dispatch_request(request: AgentRequest, state: &Arc<Mutex<AgentAbiState>>) -> AgentResponse {
    let body = if request.version != AGENT_ABI_VERSION {
        error_body(format!(
            "Agent ABI version mismatch: expected {AGENT_ABI_VERSION}, got {}",
            request.version
        ))
    } else {
        let mut state = match state.lock() {
            Ok(state) => state,
            Err(_) => {
                return AgentResponse {
                    body: error_body("Agent ABI state lock poisoned"),
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
    state: &mut AgentAbiState,
) -> Result<AgentResponseBody, String> {
    if let AgentRequestBody::Hello(hello) = body {
        if hello.auth_token != state.auth_token {
            return Err("invalid Agent ABI token".into());
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
                    "stale client {} still owns open effects; refusing unsafe session replacement",
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
        if state.sessions.len() >= AGENT_ABI_MAX_SESSIONS {
            return Err(format!(
                "Agent ABI session limit of {AGENT_ABI_MAX_SESSIONS} reached"
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
        .ok_or_else(|| "authenticated Agent ABI request requires session_id".to_string())?;
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
                && state.processes.len() >= AGENT_ABI_MAX_PROCESSES
            {
                return Err(format!(
                    "Agent ABI process registration limit of {AGENT_ABI_MAX_PROCESSES} reached"
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
            if begin.effect_id.trim().is_empty()
                || begin.kind.trim().is_empty()
                || begin.request_digest.trim().is_empty()
            {
                return Err("effect_id, kind, and request_digest must be non-empty".into());
            }
            if let Some(existing) = state.effects.get(&begin.effect_id) {
                if existing.session_id == session_id && existing.begin == begin {
                    return Ok(AgentResponseBody::EffectAccepted {
                        sequence: existing.sequence,
                    });
                }
                return Err(format!(
                    "effect {} already exists with different data",
                    begin.effect_id
                ));
            }
            if state.effects.len() >= AGENT_ABI_MAX_EFFECTS {
                return Err(format!(
                    "Agent ABI effect limit of {AGENT_ABI_MAX_EFFECTS} reached"
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
            Ok(AgentResponseBody::EffectAccepted { sequence })
        }
        AgentRequestBody::EffectComplete(completion) => {
            session_mut(state, session_id)?;
            let effect = state
                .effects
                .get_mut(&completion.effect_id)
                .ok_or_else(|| format!("effect {} was not begun", completion.effect_id))?;
            if effect.session_id != session_id {
                return Err("effect belongs to another Agent ABI session".into());
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
    state: &'a mut AgentAbiState,
    session_id: &str,
) -> Result<&'a mut ClientSession, String> {
    state
        .sessions
        .get_mut(session_id)
        .ok_or_else(|| "unknown Agent ABI session".to_string())
}

fn record_quiesced(
    state: &mut AgentAbiState,
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
        return Err("Agent ABI session still has open effects".into());
    }
    let session = session_mut(state, session_id)?;
    session.lifecycle = AgentLifecycleState::Quiesced;
    session.quiesced_checkpoint_id = Some(checkpoint_id);
    Ok(())
}

fn lock_state(state: &Arc<Mutex<AgentAbiState>>) -> std::sync::MutexGuard<'_, AgentAbiState> {
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

    fn connect(server: &AgentAbiServer) -> String {
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
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
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
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
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
            AgentAbiServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
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

        let begin = AgentEffectBegin {
            effect_id: "effect-1".into(),
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
            AgentResponseBody::EffectAccepted { sequence: 1 }
        ));
        let complete = AgentRequest::authenticated(
            &session_id,
            AgentRequestBody::EffectComplete(AgentEffectComplete {
                effect_id: "effect-1".into(),
                outcome: AgentEffectOutcome::Committed,
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
