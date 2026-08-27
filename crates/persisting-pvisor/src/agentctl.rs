//! Optional, cooperative Run-scoped AgentCtl channel owned by pVisor.

pub use persisting_agentctl::{
    AGENTCTL_MAX_FRAME_BYTES, AGENTCTL_VERSION, AgentDirective, AgentErrorCode, AgentRequest,
    AgentResponse, AgentState,
};
use persisting_agentctl::{AttemptId, RunId};
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

/// Maximum live runtime Sessions accepted by one Run.
pub const AGENTCTL_MAX_SESSIONS: usize = 64;

const SYNC_INTERVAL_MS: u64 = 1_000;

/// Diagnostic observation of one cooperative runtime client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentClientSnapshot {
    /// Stable identity supplied during `Hello`.
    pub client_id: String,
    /// Most recently accepted cooperative state.
    pub state: AgentState,
    /// Time of the most recently accepted `Sync`, if any.
    pub last_sync_unix_ms: Option<u64>,
    /// Whether the Session has missed three synchronization intervals.
    pub stale: bool,
}

/// Serializable AgentCtl observation attached to a Run Bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCtlSnapshot {
    /// Owning Run identity.
    pub run_id: String,
    /// Owning Attempt identity.
    pub attempt_id: String,
    /// Current pVisor directive.
    pub directive: AgentDirective,
    /// Runtime clients sorted by `client_id`.
    pub clients: Vec<AgentClientSnapshot>,
}

#[derive(Debug)]
struct ClientSession {
    client_id: String,
    state: AgentState,
    last_seen_unix_ms: u64,
    last_sync_unix_ms: Option<u64>,
    checkpoint_ack: Option<CheckpointAcknowledgement>,
}

#[derive(Debug, Clone, Copy)]
struct CheckpointAcknowledgement {
    generation: u64,
    acknowledged_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
struct ActiveCheckpoint {
    generation: u64,
    checkpoint_id: String,
    deadline_unix_ms: Option<u64>,
    participants: BTreeSet<String>,
}

#[derive(Debug)]
struct AgentCtlState {
    run_id: String,
    attempt_id: String,
    token: String,
    directive: AgentDirective,
    sessions: HashMap<String, ClientSession>,
    next_checkpoint_generation: u64,
    active_checkpoint: Option<ActiveCheckpoint>,
}

impl AgentCtlState {
    fn new(run_id: &RunId, attempt_id: &AttemptId, token: String) -> Self {
        Self {
            run_id: run_id.as_str().to_string(),
            attempt_id: attempt_id.as_str().to_string(),
            token,
            directive: AgentDirective::Continue,
            sessions: HashMap::new(),
            next_checkpoint_generation: 0,
            active_checkpoint: None,
        }
    }

    fn snapshot(&self) -> AgentCtlSnapshot {
        let now = crate::util::unix_now_ms();
        let mut clients = self
            .sessions
            .values()
            .map(|session| AgentClientSnapshot {
                client_id: session.client_id.clone(),
                state: session.state.clone(),
                last_sync_unix_ms: session.last_sync_unix_ms,
                stale: session_is_stale(session, now),
            })
            .collect::<Vec<_>>();
        clients.sort_by(|left, right| left.client_id.cmp(&right.client_id));
        AgentCtlSnapshot {
            run_id: self.run_id.clone(),
            attempt_id: self.attempt_id.clone(),
            directive: self.directive.clone(),
            clients,
        }
    }
}

/// Cloneable pVisor-side control surface for one Run's AgentCtl channel.
#[derive(Clone)]
pub struct AgentCtlControl {
    endpoint: PathBuf,
    state: Arc<Mutex<AgentCtlState>>,
    delegated_snapshot: Arc<Mutex<Option<AgentCtlSnapshot>>>,
}

/// Cancellation-safe ownership of one live checkpoint transition.
pub(crate) struct AgentCtlCheckpointGuard {
    control: AgentCtlControl,
    generation: u64,
    checkpoint_id: String,
}

impl AgentCtlCheckpointGuard {
    /// Capture while the exact checkpoint generation remains fully quiesced.
    ///
    /// The closure runs while directive transitions and client Sync requests
    /// are blocked. `Ok(None)` means the frozen participant set is still
    /// draining. Any completed capture, including a capture error, releases
    /// the matching `Quiesce` before returning.
    pub(crate) fn try_capture<T>(
        &self,
        capture: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<Option<T>> {
        let mut state = lock_state(&self.control.state);
        let checkpoint = state
            .active_checkpoint
            .as_ref()
            .filter(|checkpoint| checkpoint.generation == self.generation)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "checkpoint {} no longer owns the active AgentCtl quiesce",
                    self.checkpoint_id
                )
            })?;
        anyhow::ensure!(
            matches!(
                &state.directive,
                AgentDirective::Quiesce { checkpoint_id, .. }
                    if checkpoint_id == &checkpoint.checkpoint_id
            ),
            "checkpoint {} lost its AgentCtl quiesce directive",
            self.checkpoint_id
        );

        let ready = checkpoint.participants.iter().all(|session_id| {
            state.sessions.get(session_id).is_some_and(|session| {
                matches!(
                    &session.state,
                    AgentState::Quiesced { checkpoint_id }
                        if checkpoint_id == &checkpoint.checkpoint_id
                ) && session.checkpoint_ack.is_some_and(|ack| {
                    ack.generation == checkpoint.generation
                        && checkpoint
                            .deadline_unix_ms
                            .is_none_or(|deadline| ack.acknowledged_at_unix_ms <= deadline)
                })
            })
        });
        if !ready {
            if checkpoint
                .deadline_unix_ms
                .is_some_and(|deadline| crate::util::unix_now_ms() >= deadline)
            {
                anyhow::bail!(
                    "checkpoint {} timed out waiting for all AgentCtl clients to quiesce",
                    self.checkpoint_id
                );
            }
            return Ok(None);
        }

        let outcome = capture();
        finish_checkpoint(&mut state, self.generation);
        outcome.map(Some)
    }
}

impl Drop for AgentCtlCheckpointGuard {
    fn drop(&mut self) {
        finish_checkpoint(&mut lock_state(&self.control.state), self.generation);
    }
}

impl AgentCtlControl {
    /// Return the Run-local endpoint path.
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Return the latest cooperative observation.
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

    /// Freeze all currently live Sessions and request a checkpoint boundary.
    pub fn request_quiesce(
        &self,
        checkpoint_id: impl Into<String>,
        deadline_unix_ms: Option<u64>,
    ) -> anyhow::Result<()> {
        self.start_checkpoint(checkpoint_id.into(), deadline_unix_ms)
            .map(|_| ())
    }

    pub(crate) fn begin_checkpoint(
        &self,
        checkpoint_id: String,
        deadline_unix_ms: Option<u64>,
    ) -> anyhow::Result<AgentCtlCheckpointGuard> {
        let generation = self.start_checkpoint(checkpoint_id.clone(), deadline_unix_ms)?;
        Ok(AgentCtlCheckpointGuard {
            control: self.clone(),
            generation,
            checkpoint_id,
        })
    }

    fn start_checkpoint(
        &self,
        checkpoint_id: String,
        deadline_unix_ms: Option<u64>,
    ) -> anyhow::Result<u64> {
        anyhow::ensure!(
            !checkpoint_id.trim().is_empty(),
            "AgentCtl checkpoint_id must be non-empty"
        );
        let mut state = lock_state(&self.state);
        anyhow::ensure!(
            matches!(state.directive, AgentDirective::Continue),
            "AgentCtl cannot start a checkpoint while {:?} is active",
            state.directive
        );
        let now = crate::util::unix_now_ms();
        state
            .sessions
            .retain(|_, session| !session_is_stale(session, now));
        anyhow::ensure!(
            !state.sessions.is_empty(),
            "live checkpoint requires at least one AgentCtl client"
        );
        let generation = state
            .next_checkpoint_generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("AgentCtl checkpoint generation exhausted"))?;
        state.next_checkpoint_generation = generation;
        for session in state.sessions.values_mut() {
            session.checkpoint_ack = None;
        }
        state.active_checkpoint = Some(ActiveCheckpoint {
            generation,
            checkpoint_id: checkpoint_id.clone(),
            deadline_unix_ms,
            participants: state.sessions.keys().cloned().collect(),
        });
        state.directive = AgentDirective::Quiesce {
            checkpoint_id,
            deadline_unix_ms,
        };
        Ok(generation)
    }

    /// Release clients after a checkpoint succeeds or is abandoned.
    pub fn continue_execution(&self) {
        let mut state = lock_state(&self.state);
        if let Some(checkpoint) = &state.active_checkpoint {
            let generation = checkpoint.generation;
            finish_checkpoint(&mut state, generation);
        }
    }

    /// Ask all runtime clients to terminate.
    pub fn request_shutdown(&self, reason: Option<String>) {
        let mut state = lock_state(&self.state);
        state.active_checkpoint = None;
        for session in state.sessions.values_mut() {
            session.checkpoint_ack = None;
        }
        state.directive = AgentDirective::Shutdown { reason };
    }
}

fn finish_checkpoint(state: &mut AgentCtlState, generation: u64) {
    if state
        .active_checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.generation == generation)
    {
        state.active_checkpoint = None;
        for session in state.sessions.values_mut() {
            session.checkpoint_ack = None;
        }
        state.directive = AgentDirective::Continue;
    }
}

/// Owns the Run-scoped Unix listener and removes it on drop.
pub struct AgentCtlServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    socket_path: PathBuf,
    token: String,
    control: AgentCtlControl,
}

impl AgentCtlServer {
    /// Create and start one Run-scoped AgentCtl server.
    pub fn start(run_id: &RunId, attempt_id: &AttemptId) -> anyhow::Result<Self> {
        // macOS `sockaddr_un` paths are capped at SUN_LEN (~104 bytes), so bind in
        // the fixed, short `/tmp` directory rather than `std::env::temp_dir()`,
        // which can point at a deep per-user path (e.g. `/var/folders/.../T/`).
        let socket_path = Path::new("/tmp").join(format!(
            "pvisor-agent-{}.sock",
            uuid::Uuid::new_v4().simple()
        ));
        let token = uuid::Uuid::new_v4().to_string();
        let state = Arc::new(Mutex::new(AgentCtlState::new(
            run_id,
            attempt_id,
            token.clone(),
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
            token,
            control,
        })
    }

    /// Return the cloneable Run control surface.
    pub fn control(&self) -> AgentCtlControl {
        self.control.clone()
    }

    /// Return the environment injected into runtime clients.
    pub fn environment(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                persisting_agentctl::AGENTCTL_ENDPOINT_ENV.into(),
                self.socket_path.display().to_string(),
            ),
            (
                persisting_agentctl::AGENTCTL_TOKEN_ENV.into(),
                self.token.clone(),
            ),
            (
                persisting_agentctl::AGENTCTL_VERSION_ENV.into(),
                AGENTCTL_VERSION.to_string(),
            ),
            (
                persisting_agentctl::AGENTCTL_TRANSPORT_ENV.into(),
                "unix".into(),
            ),
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

#[derive(Debug)]
struct ProtocolFailure {
    code: AgentErrorCode,
    message: String,
}

impl ProtocolFailure {
    fn new(code: AgentErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn serve_connection(mut stream: std::os::unix::net::UnixStream, state: &Arc<Mutex<AgentCtlState>>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let response = read_request(&stream)
        .map_err(|error| ProtocolFailure::new(AgentErrorCode::InvalidRequest, error.to_string()))
        .and_then(|request| dispatch_request(request, state))
        .unwrap_or_else(error_response);
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

fn dispatch_request(
    request: AgentRequest,
    state: &Arc<Mutex<AgentCtlState>>,
) -> Result<AgentResponse, ProtocolFailure> {
    let version = match &request {
        AgentRequest::Hello { version, .. } | AgentRequest::Sync { version, .. } => *version,
    };
    if version != AGENTCTL_VERSION {
        return Err(ProtocolFailure::new(
            AgentErrorCode::VersionMismatch,
            format!("AgentCtl version mismatch: expected {AGENTCTL_VERSION}, got {version}"),
        ));
    }
    let mut state = state.lock().map_err(|_| {
        ProtocolFailure::new(AgentErrorCode::Conflict, "AgentCtl state lock poisoned")
    })?;
    handle_request(request, &mut state)
}

fn handle_request(
    request: AgentRequest,
    state: &mut AgentCtlState,
) -> Result<AgentResponse, ProtocolFailure> {
    match request {
        AgentRequest::Hello {
            token, client_id, ..
        } => handle_hello(token, client_id, state),
        AgentRequest::Sync {
            session_id,
            state: reported_state,
            ..
        } => handle_sync(session_id, reported_state, state),
    }
}

fn handle_hello(
    token: String,
    client_id: String,
    state: &mut AgentCtlState,
) -> Result<AgentResponse, ProtocolFailure> {
    if token != state.token {
        return Err(ProtocolFailure::new(
            AgentErrorCode::Unauthorized,
            "invalid AgentCtl token",
        ));
    }
    if client_id.trim().is_empty() {
        return Err(ProtocolFailure::new(
            AgentErrorCode::InvalidRequest,
            "client_id must be non-empty",
        ));
    }
    if matches!(state.directive, AgentDirective::Quiesce { .. }) {
        return Err(ProtocolFailure::new(
            AgentErrorCode::Conflict,
            "AgentCtl checkpoint is in progress",
        ));
    }

    let now = crate::util::unix_now_ms();
    state
        .sessions
        .retain(|_, session| !session_is_stale(session, now));
    if state
        .sessions
        .values()
        .any(|session| session.client_id == client_id)
    {
        return Err(ProtocolFailure::new(
            AgentErrorCode::Conflict,
            format!("client {client_id} already has a live Session"),
        ));
    }
    if state.sessions.len() >= AGENTCTL_MAX_SESSIONS {
        return Err(ProtocolFailure::new(
            AgentErrorCode::Conflict,
            format!("AgentCtl Session limit of {AGENTCTL_MAX_SESSIONS} reached"),
        ));
    }

    let session_id = uuid::Uuid::new_v4().to_string();
    state.sessions.insert(
        session_id.clone(),
        ClientSession {
            client_id,
            state: AgentState::Active,
            last_seen_unix_ms: now,
            last_sync_unix_ms: None,
            checkpoint_ack: None,
        },
    );
    Ok(AgentResponse::Welcome {
        session_id,
        sync_interval_ms: SYNC_INTERVAL_MS,
        directive: state.directive.clone(),
    })
}

fn handle_sync(
    session_id: String,
    reported_state: AgentState,
    state: &mut AgentCtlState,
) -> Result<AgentResponse, ProtocolFailure> {
    let session = state.sessions.get(&session_id).ok_or_else(|| {
        ProtocolFailure::new(AgentErrorCode::Unauthorized, "unknown AgentCtl Session")
    })?;
    let active_checkpoint = state.active_checkpoint.as_ref().map(|checkpoint| {
        (
            checkpoint.generation,
            checkpoint.checkpoint_id.clone(),
            checkpoint.participants.contains(&session_id),
        )
    });
    if let AgentState::Quiesced { checkpoint_id } = &reported_state {
        let repeated = session.state == reported_state;
        let matches_active = active_checkpoint
            .as_ref()
            .is_some_and(|(_, active, participant)| *participant && active == checkpoint_id);
        if !repeated && !matches_active {
            return Err(ProtocolFailure::new(
                AgentErrorCode::Conflict,
                "quiesced state does not match the active checkpoint",
            ));
        }
    }

    let now = crate::util::unix_now_ms();
    let session = state
        .sessions
        .get_mut(&session_id)
        .expect("Session validated");
    session.checkpoint_ack = match (&reported_state, active_checkpoint) {
        (
            AgentState::Quiesced { checkpoint_id },
            Some((generation, active_checkpoint_id, true)),
        ) if checkpoint_id == &active_checkpoint_id => Some(match session.checkpoint_ack {
            Some(ack) if ack.generation == generation => ack,
            _ => CheckpointAcknowledgement {
                generation,
                acknowledged_at_unix_ms: now,
            },
        }),
        _ => None,
    };
    session.state = reported_state;
    session.last_seen_unix_ms = now;
    session.last_sync_unix_ms = Some(now);
    Ok(AgentResponse::Synced {
        directive: state.directive.clone(),
    })
}

fn session_is_stale(session: &ClientSession, now_unix_ms: u64) -> bool {
    now_unix_ms.saturating_sub(session.last_seen_unix_ms) > SYNC_INTERVAL_MS * 3
}

fn lock_state(state: &Arc<Mutex<AgentCtlState>>) -> std::sync::MutexGuard<'_, AgentCtlState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn error_response(error: ProtocolFailure) -> AgentResponse {
    AgentResponse::Error {
        code: error.code,
        message: error.message,
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

    fn connect(server: &AgentCtlServer, client_id: &str) -> String {
        match exchange(
            &server.socket_path,
            &AgentRequest::Hello {
                version: AGENTCTL_VERSION,
                token: server.token.clone(),
                client_id: client_id.into(),
            },
        ) {
            AgentResponse::Welcome { session_id, .. } => session_id,
            response => panic!("unexpected response: {response:?}"),
        }
    }

    #[test]
    fn rejects_invalid_token_and_protocol_version_with_typed_codes() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        assert!(matches!(
            exchange(
                &server.socket_path,
                &AgentRequest::Hello {
                    version: AGENTCTL_VERSION,
                    token: "wrong".into(),
                    client_id: "client".into(),
                },
            ),
            AgentResponse::Error {
                code: AgentErrorCode::Unauthorized,
                ..
            }
        ));
        assert!(matches!(
            exchange(
                &server.socket_path,
                &AgentRequest::Hello {
                    version: 99,
                    token: server.token.clone(),
                    client_id: "client".into(),
                },
            ),
            AgentResponse::Error {
                code: AgentErrorCode::VersionMismatch,
                ..
            }
        ));
    }

    #[test]
    fn request_quiesce_purges_sessions_that_were_already_stale() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let stale_id = connect(&server, "stale");
        let live_id = connect(&server, "live");
        {
            let mut state = lock_state(&server.control.state);
            state.sessions.get_mut(&stale_id).unwrap().last_seen_unix_ms = 0;
            state.sessions.get_mut(&live_id).unwrap().last_seen_unix_ms =
                crate::util::unix_now_ms();
        }

        server.control.request_quiesce("cp", None).unwrap();
        let snapshot = server.control.snapshot();
        assert_eq!(snapshot.clients.len(), 1);
        assert_eq!(snapshot.clients[0].client_id, "live");
    }

    #[test]
    fn checkpoint_never_expires_a_frozen_participant() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server, "participant");
        server.control.request_quiesce("cp", None).unwrap();
        lock_state(&server.control.state)
            .sessions
            .get_mut(&session_id)
            .unwrap()
            .last_seen_unix_ms = 0;

        let snapshot = server.control.snapshot();
        assert_eq!(snapshot.clients.len(), 1);
        assert!(snapshot.clients[0].stale);
    }

    #[test]
    fn delegated_snapshot_preserves_outer_identity() {
        let server =
            AgentCtlServer::start(&RunId::new("outer-run"), &AttemptId::new("outer-attempt"))
                .unwrap();
        let delegated = AgentCtlSnapshot {
            run_id: "inner-run".into(),
            attempt_id: "inner-attempt".into(),
            directive: AgentDirective::Shutdown { reason: None },
            clients: Vec::new(),
        };
        server.control.import_delegated_snapshot(delegated);

        let imported = server.control.snapshot();
        assert_eq!(imported.run_id, "outer-run");
        assert_eq!(imported.attempt_id, "outer-attempt");
        assert!(matches!(
            imported.directive,
            AgentDirective::Shutdown { .. }
        ));
    }

    #[test]
    fn retrying_a_checkpoint_id_requires_a_fresh_acknowledgement() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server, "participant");
        let deadline = crate::util::unix_now_ms().saturating_add(10_000);
        let first = server
            .control
            .begin_checkpoint("cp".into(), Some(deadline))
            .unwrap();
        let quiesced = AgentState::Quiesced {
            checkpoint_id: "cp".into(),
        };
        assert!(matches!(
            exchange(
                &server.socket_path,
                &AgentRequest::Sync {
                    version: AGENTCTL_VERSION,
                    session_id: session_id.clone(),
                    state: quiesced.clone(),
                },
            ),
            AgentResponse::Synced {
                directive: AgentDirective::Quiesce { .. }
            }
        ));
        drop(first);

        let retry = server
            .control
            .begin_checkpoint("cp".into(), Some(deadline))
            .unwrap();
        assert!(retry.try_capture(|| Ok(())).unwrap().is_none());

        exchange(
            &server.socket_path,
            &AgentRequest::Sync {
                version: AGENTCTL_VERSION,
                session_id,
                state: quiesced,
            },
        );
        assert_eq!(retry.try_capture(|| Ok(())).unwrap(), Some(()));
    }

    #[test]
    fn checkpoint_guard_drop_releases_only_its_own_quiesce() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        connect(&server, "participant");
        let checkpoint = server.control.begin_checkpoint("cp".into(), None).unwrap();
        drop(checkpoint);
        assert_eq!(
            server.control.snapshot().directive,
            AgentDirective::Continue
        );

        let checkpoint = server
            .control
            .begin_checkpoint("cp-2".into(), None)
            .unwrap();
        server.control.request_shutdown(Some("stop".into()));
        drop(checkpoint);
        assert!(matches!(
            server.control.snapshot().directive,
            AgentDirective::Shutdown { .. }
        ));
        server.control.continue_execution();
        assert!(matches!(
            server.control.snapshot().directive,
            AgentDirective::Shutdown { .. }
        ));
    }

    #[test]
    fn checkpoint_copy_holds_transition_ownership_until_capture_finishes() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server, "participant");
        let checkpoint = server.control.begin_checkpoint("cp".into(), None).unwrap();
        exchange(
            &server.socket_path,
            &AgentRequest::Sync {
                version: AGENTCTL_VERSION,
                session_id,
                state: AgentState::Quiesced {
                    checkpoint_id: "cp".into(),
                },
            },
        );

        let (capture_started_tx, capture_started_rx) = std::sync::mpsc::channel();
        let (release_capture_tx, release_capture_rx) = std::sync::mpsc::channel();
        let capture = std::thread::spawn(move || {
            checkpoint
                .try_capture(|| {
                    capture_started_tx.send(()).unwrap();
                    release_capture_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap()
        });
        capture_started_rx.recv().unwrap();

        let control = server.control();
        let (shutdown_done_tx, shutdown_done_rx) = std::sync::mpsc::channel();
        let shutdown = std::thread::spawn(move || {
            control.request_shutdown(Some("stop".into()));
            shutdown_done_tx.send(()).unwrap();
        });
        assert!(
            shutdown_done_rx
                .recv_timeout(Duration::from_millis(30))
                .is_err()
        );

        release_capture_tx.send(()).unwrap();
        assert_eq!(capture.join().unwrap(), Some(()));
        shutdown_done_rx.recv().unwrap();
        shutdown.join().unwrap();
        assert!(matches!(
            server.control.snapshot().directive,
            AgentDirective::Shutdown { .. }
        ));
    }

    #[test]
    fn acknowledgement_after_deadline_cannot_satisfy_checkpoint() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        let session_id = connect(&server, "participant");
        let deadline = crate::util::unix_now_ms();
        let checkpoint = server
            .control
            .begin_checkpoint("cp".into(), Some(deadline))
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        exchange(
            &server.socket_path,
            &AgentRequest::Sync {
                version: AGENTCTL_VERSION,
                session_id,
                state: AgentState::Quiesced {
                    checkpoint_id: "cp".into(),
                },
            },
        );

        let error = checkpoint.try_capture(|| Ok(())).unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }

    #[test]
    fn hello_purges_all_stale_sessions_before_enforcing_capacity() {
        let server =
            AgentCtlServer::start(&RunId::new("run-1"), &AttemptId::new("attempt-1")).unwrap();
        for index in 0..AGENTCTL_MAX_SESSIONS {
            connect(&server, &format!("stale-{index}"));
        }
        for session in lock_state(&server.control.state).sessions.values_mut() {
            session.last_seen_unix_ms = 0;
        }

        connect(&server, "replacement");
        let snapshot = server.control.snapshot();
        assert_eq!(snapshot.clients.len(), 1);
        assert_eq!(snapshot.clients[0].client_id, "replacement");
    }
}
