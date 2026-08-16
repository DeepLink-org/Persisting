//! Lightweight, versioned control-plane client for the standalone pChronicle process.
//!
//! This optional module contains no storage engine. pChronicle implements the
//! durable operations; orchestrators such as pPilot depend only on these
//! contracts and the long-lived process transport.

use crate::EventRecord;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use persisting_agentctl::{
    AttemptId, RunCommit, RunCommitRequest, RunControlRecord, RunId, RunLeaseRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub const CHRONICLE_CONTROL_VERSION: u32 = 2;
pub const CHRONICLE_CONTROL_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseAcquireOutcome {
    Acquired(RunLeaseRecord),
    Held(RunLeaseRecord),
    AlreadyCommitted(RunCommit),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitRunOutcome {
    Committed(RunCommit),
    AlreadyCommitted(RunCommit),
    StaleLease {
        supplied_epoch: u64,
        current_epoch: Option<u64>,
    },
    Conflict(RunCommit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptRecordState {
    Active,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub revision: u64,
    pub run_id: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
    pub state: AttemptRecordState,
    pub heartbeat_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_result: Option<Value>,
}

impl AttemptRecord {
    pub fn is_live_at(&self, now_unix_ms: u64) -> bool {
        self.state == AttemptRecordState::Active && self.expires_at_unix_ms > now_unix_ms
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryAppendRequest {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    #[serde(default)]
    pub root_session_id: Option<String>,
    pub records: Vec<EventRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryAppendResponse {
    pub storage: String,
    pub agent_id: String,
    pub session_id: String,
    pub accepted_records: usize,
    pub dataset: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChronicleControlEnvelope {
    pub version: u32,
    pub request_id: u64,
    pub auth_token: String,
    pub request: ChronicleControlRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ChronicleControlRequest {
    Ping,
    AcquireLease {
        run_id: RunId,
        task_id: Option<String>,
        owner: String,
        ttl_ms: u64,
    },
    TakeoverLease {
        run_id: RunId,
        task_id: Option<String>,
        owner: String,
        ttl_ms: u64,
    },
    BindAttempt {
        run_id: RunId,
        epoch: u64,
        attempt_id: AttemptId,
    },
    RenewLease {
        run_id: RunId,
        epoch: u64,
        owner: String,
        ttl_ms: u64,
    },
    CommitRun(RunCommitRequest),
    GetRun {
        run_id: RunId,
    },
    ListRuns,
    GetAttempt {
        run_id: String,
    },
    PublishAttemptActive {
        run_id: String,
        attempt_id: String,
        lease_epoch: u64,
        ttl_ms: u64,
    },
    HeartbeatAttempt {
        run_id: String,
        attempt_id: String,
        lease_epoch: u64,
        ttl_ms: u64,
    },
    PublishAttemptTerminal {
        run_id: String,
        attempt_id: String,
        lease_epoch: u64,
        result: Value,
    },
    AppendTrajectory(TrajectoryAppendRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChronicleControlResponseEnvelope {
    pub version: u32,
    pub request_id: u64,
    pub response: ChronicleControlResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChronicleControlReady {
    pub version: u32,
    pub endpoint: String,
    pub auth_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ChronicleControlResponse {
    Pong,
    LeaseAcquire(LeaseAcquireOutcome),
    CommitRun(CommitRunOutcome),
    Boolean(bool),
    Run(Option<RunControlRecord>),
    Runs(Vec<RunControlRecord>),
    Attempt(Option<AttemptRecord>),
    TrajectoryAppend(TrajectoryAppendResponse),
    Error { message: String },
}

#[async_trait]
pub trait ChronicleControl: Send + Sync {
    fn root_uri(&self) -> &str;

    async fn acquire_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome>;
    async fn takeover_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome>;
    async fn bind_attempt(&self, run_id: &RunId, epoch: u64, attempt_id: AttemptId)
        -> Result<bool>;
    async fn renew_lease(
        &self,
        run_id: &RunId,
        epoch: u64,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<bool>;
    async fn commit_run(&self, request: RunCommitRequest) -> Result<CommitRunOutcome>;
    async fn get_run(&self, run_id: &RunId) -> Result<Option<RunControlRecord>>;
    async fn list_runs(&self) -> Result<Vec<RunControlRecord>>;
    async fn get_attempt(&self, run_id: &str) -> Result<Option<AttemptRecord>>;
    async fn publish_attempt_active(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool>;
    async fn heartbeat_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool>;
    async fn publish_attempt_terminal(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        result: Value,
    ) -> Result<bool>;
    async fn append_trajectory(
        &self,
        request: TrajectoryAppendRequest,
    ) -> Result<TrajectoryAppendResponse>;
}

struct ProcessState {
    child: Mutex<Child>,
    next_request_id: AtomicU64,
}

impl Drop for ProcessState {
    fn drop(&mut self) {
        let _ = self.child.get_mut().start_kill();
    }
}

#[derive(Clone)]
pub struct ChronicleControlProcessClient {
    root_uri: String,
    binary: PathBuf,
    endpoint: SocketAddr,
    auth_token: String,
    state: Arc<ProcessState>,
}

impl std::fmt::Debug for ChronicleControlProcessClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChronicleControlProcessClient")
            .field("root_uri", &self.root_uri)
            .field("binary", &self.binary)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ChronicleControlProcessClient {
    pub async fn spawn(binary: impl AsRef<Path>, root_uri: impl Into<String>) -> Result<Self> {
        let requested_binary = binary.as_ref().to_path_buf();
        let binary = if requested_binary.components().count() == 1 {
            std::env::current_exe()
                .ok()
                .and_then(|current| {
                    let parent = current.parent()?;
                    let sibling = parent.join(&requested_binary);
                    if sibling.is_file() {
                        return Some(sibling);
                    }
                    parent
                        .parent()
                        .map(|profile| profile.join(&requested_binary))
                        .filter(|candidate| candidate.is_file())
                })
                .unwrap_or(requested_binary)
        } else {
            requested_binary
        };
        let root_uri = root_uri.into();
        let mut child = Command::new(&binary)
            .arg("control")
            .arg("--storage")
            .arg(&root_uri)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn pChronicle control process {}", binary.display()))?;
        let stdout = child
            .stdout
            .take()
            .context("pChronicle control stdout unavailable")?;
        let mut stdout = BufReader::new(stdout);
        let mut ready = String::new();
        let bytes = stdout
            .read_line(&mut ready)
            .await
            .context("read pChronicle control endpoint")?;
        anyhow::ensure!(
            bytes > 0,
            "pChronicle control process exited before readiness"
        );
        let ready: ChronicleControlReady =
            serde_json::from_str(&ready).context("decode pChronicle readiness")?;
        anyhow::ensure!(
            ready.version == CHRONICLE_CONTROL_VERSION,
            "unsupported pChronicle control version {}",
            ready.version
        );
        let endpoint = ready
            .endpoint
            .parse::<SocketAddr>()
            .context("parse pChronicle control endpoint")?;
        anyhow::ensure!(
            endpoint.ip().is_loopback(),
            "pChronicle returned a non-loopback endpoint"
        );
        let client = Self {
            root_uri,
            binary,
            endpoint,
            auth_token: ready.auth_token,
            state: Arc::new(ProcessState {
                child: Mutex::new(child),
                next_request_id: AtomicU64::new(1),
            }),
        };
        match client.call(ChronicleControlRequest::Ping).await? {
            ChronicleControlResponse::Pong => Ok(client),
            other => bail!("unexpected pChronicle handshake response: {other:?}"),
        }
    }

    async fn call(&self, request: ChronicleControlRequest) -> Result<ChronicleControlResponse> {
        let request_id = self.state.next_request_id.fetch_add(1, Ordering::Relaxed);
        let envelope = ChronicleControlEnvelope {
            version: CHRONICLE_CONTROL_VERSION,
            request_id,
            auth_token: self.auth_token.clone(),
            request,
        };
        let mut frame = serde_json::to_vec(&envelope).context("encode pChronicle request")?;
        anyhow::ensure!(
            frame.len() <= CHRONICLE_CONTROL_MAX_FRAME_BYTES,
            "pChronicle request frame exceeds {} bytes",
            CHRONICLE_CONTROL_MAX_FRAME_BYTES
        );
        frame.push(b'\n');
        let stream = TcpStream::connect(self.endpoint)
            .await
            .with_context(|| format!("connect pChronicle control at {}", self.endpoint))?;
        stream
            .set_nodelay(true)
            .context("configure pChronicle control socket")?;
        let (read, mut write) = stream.into_split();
        write
            .write_all(&frame)
            .await
            .context("write pChronicle request")?;
        write.flush().await.context("flush pChronicle request")?;

        let mut line = String::new();
        let bytes = BufReader::new(read)
            .read_line(&mut line)
            .await
            .context("read pChronicle response")?;
        if bytes == 0 {
            let status = self
                .state
                .child
                .lock()
                .await
                .try_wait()
                .context("inspect pChronicle process")?;
            bail!("pChronicle control process closed stdout (status: {status:?})");
        }
        anyhow::ensure!(
            bytes <= CHRONICLE_CONTROL_MAX_FRAME_BYTES,
            "pChronicle response frame exceeds {} bytes",
            CHRONICLE_CONTROL_MAX_FRAME_BYTES
        );
        let response: ChronicleControlResponseEnvelope =
            serde_json::from_str(&line).context("decode pChronicle response")?;
        anyhow::ensure!(
            response.version == CHRONICLE_CONTROL_VERSION,
            "unsupported pChronicle control version {}",
            response.version
        );
        anyhow::ensure!(
            response.request_id == request_id,
            "pChronicle response id mismatch: expected {request_id}, got {}",
            response.request_id
        );
        match response.response {
            ChronicleControlResponse::Error { message } => bail!("pChronicle: {message}"),
            response => Ok(response),
        }
    }
}

macro_rules! expect_response {
    ($value:expr, $variant:path, $name:literal) => {
        match $value {
            $variant(value) => Ok(value),
            other => bail!("unexpected pChronicle response to {}: {other:?}", $name),
        }
    };
}

#[async_trait]
impl ChronicleControl for ChronicleControlProcessClient {
    fn root_uri(&self) -> &str {
        &self.root_uri
    }

    async fn acquire_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome> {
        expect_response!(
            self.call(ChronicleControlRequest::AcquireLease {
                run_id: run_id.clone(),
                task_id: task_id.map(str::to_owned),
                owner: owner.to_owned(),
                ttl_ms
            })
            .await?,
            ChronicleControlResponse::LeaseAcquire,
            "acquire_lease"
        )
    }
    async fn takeover_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome> {
        expect_response!(
            self.call(ChronicleControlRequest::TakeoverLease {
                run_id: run_id.clone(),
                task_id: task_id.map(str::to_owned),
                owner: owner.to_owned(),
                ttl_ms
            })
            .await?,
            ChronicleControlResponse::LeaseAcquire,
            "takeover_lease"
        )
    }
    async fn bind_attempt(
        &self,
        run_id: &RunId,
        epoch: u64,
        attempt_id: AttemptId,
    ) -> Result<bool> {
        expect_response!(
            self.call(ChronicleControlRequest::BindAttempt {
                run_id: run_id.clone(),
                epoch,
                attempt_id
            })
            .await?,
            ChronicleControlResponse::Boolean,
            "bind_attempt"
        )
    }
    async fn renew_lease(
        &self,
        run_id: &RunId,
        epoch: u64,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<bool> {
        expect_response!(
            self.call(ChronicleControlRequest::RenewLease {
                run_id: run_id.clone(),
                epoch,
                owner: owner.to_owned(),
                ttl_ms
            })
            .await?,
            ChronicleControlResponse::Boolean,
            "renew_lease"
        )
    }
    async fn commit_run(&self, request: RunCommitRequest) -> Result<CommitRunOutcome> {
        expect_response!(
            self.call(ChronicleControlRequest::CommitRun(request))
                .await?,
            ChronicleControlResponse::CommitRun,
            "commit_run"
        )
    }
    async fn get_run(&self, run_id: &RunId) -> Result<Option<RunControlRecord>> {
        expect_response!(
            self.call(ChronicleControlRequest::GetRun {
                run_id: run_id.clone()
            })
            .await?,
            ChronicleControlResponse::Run,
            "get_run"
        )
    }
    async fn list_runs(&self) -> Result<Vec<RunControlRecord>> {
        expect_response!(
            self.call(ChronicleControlRequest::ListRuns).await?,
            ChronicleControlResponse::Runs,
            "list_runs"
        )
    }
    async fn get_attempt(&self, run_id: &str) -> Result<Option<AttemptRecord>> {
        expect_response!(
            self.call(ChronicleControlRequest::GetAttempt {
                run_id: run_id.to_owned()
            })
            .await?,
            ChronicleControlResponse::Attempt,
            "get_attempt"
        )
    }
    async fn publish_attempt_active(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool> {
        expect_response!(
            self.call(ChronicleControlRequest::PublishAttemptActive {
                run_id: run_id.to_owned(),
                attempt_id: attempt_id.to_owned(),
                lease_epoch,
                ttl_ms
            })
            .await?,
            ChronicleControlResponse::Boolean,
            "publish_attempt_active"
        )
    }

    async fn heartbeat_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool> {
        expect_response!(
            self.call(ChronicleControlRequest::HeartbeatAttempt {
                run_id: run_id.to_owned(),
                attempt_id: attempt_id.to_owned(),
                lease_epoch,
                ttl_ms
            })
            .await?,
            ChronicleControlResponse::Boolean,
            "heartbeat_attempt"
        )
    }

    async fn publish_attempt_terminal(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        result: Value,
    ) -> Result<bool> {
        expect_response!(
            self.call(ChronicleControlRequest::PublishAttemptTerminal {
                run_id: run_id.to_owned(),
                attempt_id: attempt_id.to_owned(),
                lease_epoch,
                result
            })
            .await?,
            ChronicleControlResponse::Boolean,
            "publish_attempt_terminal"
        )
    }
    async fn append_trajectory(
        &self,
        request: TrajectoryAppendRequest,
    ) -> Result<TrajectoryAppendResponse> {
        expect_response!(
            self.call(ChronicleControlRequest::AppendTrajectory(request))
                .await?,
            ChronicleControlResponse::TrajectoryAppend,
            "append_trajectory"
        )
    }
}

/// Deterministic embedded control implementation for orchestration tests and
/// callers that explicitly do not need persistence across process restarts.
pub struct MemoryChronicleControl {
    root_uri: String,
    runs: StdMutex<BTreeMap<RunId, RunControlRecord>>,
    attempts: StdMutex<BTreeMap<String, AttemptRecord>>,
}

impl std::fmt::Debug for MemoryChronicleControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryChronicleControl")
            .field("root_uri", &self.root_uri)
            .finish_non_exhaustive()
    }
}

impl MemoryChronicleControl {
    pub fn new(root_uri: impl Into<String>) -> Self {
        Self {
            root_uri: root_uri.into(),
            runs: StdMutex::new(BTreeMap::new()),
            attempts: StdMutex::new(BTreeMap::new()),
        }
    }

    fn acquire_inner(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
        force: bool,
    ) -> Result<LeaseAcquireOutcome> {
        anyhow::ensure!(
            !owner.trim().is_empty(),
            "Run lease owner must not be empty"
        );
        let now = unix_now_ms();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?;
        let current = runs.get(run_id).cloned();
        if let Some(commit) = current.as_ref().and_then(|record| record.commit.clone()) {
            return Ok(LeaseAcquireOutcome::AlreadyCommitted(commit));
        }
        if let Some(existing) = current.as_ref().and_then(|record| record.lease.as_ref()) {
            if existing.owner != owner && existing.expires_at_unix_ms > now && !force {
                return Ok(LeaseAcquireOutcome::Held(existing.clone()));
            }
            if existing.owner == owner && !force {
                let mut lease = existing.clone();
                lease.expires_at_unix_ms = now.saturating_add(ttl_ms.max(1));
                let mut record = current.expect("lease requires record");
                record.revision = record.revision.saturating_add(1);
                record.lease = Some(lease.clone());
                runs.insert(run_id.clone(), record);
                return Ok(LeaseAcquireOutcome::Acquired(lease));
            }
        }
        let epoch = current
            .as_ref()
            .and_then(|record| record.lease.as_ref().map(|lease| lease.epoch))
            .unwrap_or(0)
            .checked_add(1)
            .context("Run lease epoch overflow")?;
        let lease = RunLeaseRecord {
            run_id: run_id.clone(),
            task_id: task_id.map(str::to_owned),
            epoch,
            owner: owner.to_owned(),
            issued_at_unix_ms: now,
            expires_at_unix_ms: now.saturating_add(ttl_ms.max(1)),
            attempt_id: None,
        };
        let revision = current
            .as_ref()
            .map_or(1, |record| record.revision.saturating_add(1));
        runs.insert(
            run_id.clone(),
            RunControlRecord {
                revision,
                run_id: run_id.clone(),
                lease: Some(lease.clone()),
                commit: None,
            },
        );
        Ok(LeaseAcquireOutcome::Acquired(lease))
    }
}

#[async_trait]
impl ChronicleControl for MemoryChronicleControl {
    fn root_uri(&self) -> &str {
        &self.root_uri
    }

    async fn acquire_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome> {
        self.acquire_inner(run_id, task_id, owner, ttl_ms, false)
    }

    async fn takeover_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<LeaseAcquireOutcome> {
        self.acquire_inner(run_id, task_id, owner, ttl_ms, true)
    }

    async fn bind_attempt(
        &self,
        run_id: &RunId,
        epoch: u64,
        attempt_id: AttemptId,
    ) -> Result<bool> {
        let now = unix_now_ms();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?;
        let Some(record) = runs.get_mut(run_id) else {
            return Ok(false);
        };
        let Some(lease) = record.lease.as_mut() else {
            return Ok(false);
        };
        if record.commit.is_some() || lease.epoch != epoch || lease.expires_at_unix_ms <= now {
            return Ok(false);
        }
        if lease
            .attempt_id
            .as_ref()
            .is_some_and(|current| current != &attempt_id)
        {
            return Ok(false);
        }
        if lease.attempt_id.is_none() {
            lease.attempt_id = Some(attempt_id);
            record.revision = record.revision.saturating_add(1);
        }
        Ok(true)
    }

    async fn renew_lease(
        &self,
        run_id: &RunId,
        epoch: u64,
        owner: &str,
        ttl_ms: u64,
    ) -> Result<bool> {
        let now = unix_now_ms();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?;
        let Some(record) = runs.get_mut(run_id) else {
            return Ok(false);
        };
        let Some(lease) = record.lease.as_mut() else {
            return Ok(false);
        };
        if record.commit.is_some()
            || lease.epoch != epoch
            || lease.owner != owner
            || lease.expires_at_unix_ms <= now
        {
            return Ok(false);
        }
        lease.expires_at_unix_ms = now.saturating_add(ttl_ms.max(1));
        record.revision = record.revision.saturating_add(1);
        Ok(true)
    }

    async fn commit_run(&self, request: RunCommitRequest) -> Result<CommitRunOutcome> {
        anyhow::ensure!(
            request.state.is_terminal(),
            "RunCommit requires a terminal Run state"
        );
        anyhow::ensure!(
            request.lease_epoch > 0,
            "RunCommit requires a non-zero lease epoch"
        );
        anyhow::ensure!(
            !request.result_digest.trim().is_empty(),
            "RunCommit result_digest must not be empty"
        );
        let now = unix_now_ms();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?;
        let Some(record) = runs.get_mut(&request.run_id) else {
            return Ok(CommitRunOutcome::StaleLease {
                supplied_epoch: request.lease_epoch,
                current_epoch: None,
            });
        };
        if let Some(existing) = record.commit.clone() {
            return Ok(if existing.request == request {
                CommitRunOutcome::AlreadyCommitted(existing)
            } else {
                CommitRunOutcome::Conflict(existing)
            });
        }
        let current_epoch = record.lease.as_ref().map(|lease| lease.epoch);
        let matches = record.lease.as_ref().is_some_and(|lease| {
            lease.epoch == request.lease_epoch
                && lease.expires_at_unix_ms > now
                && lease
                    .attempt_id
                    .as_ref()
                    .is_none_or(|attempt| attempt == &request.attempt_id)
        });
        if !matches {
            return Ok(CommitRunOutcome::StaleLease {
                supplied_epoch: request.lease_epoch,
                current_epoch,
            });
        }
        let commit = RunCommit {
            request: request.clone(),
            committed_at_unix_ms: now,
        };
        if let Some(lease) = record.lease.as_mut() {
            lease.attempt_id.get_or_insert(request.attempt_id.clone());
        }
        record.commit = Some(commit.clone());
        record.revision = record.revision.saturating_add(1);
        Ok(CommitRunOutcome::Committed(commit))
    }

    async fn get_run(&self, run_id: &RunId) -> Result<Option<RunControlRecord>> {
        Ok(self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?
            .get(run_id)
            .cloned())
    }

    async fn list_runs(&self) -> Result<Vec<RunControlRecord>> {
        Ok(self
            .runs
            .lock()
            .map_err(|_| anyhow::anyhow!("Run control lock poisoned"))?
            .values()
            .cloned()
            .collect())
    }

    async fn get_attempt(&self, run_id: &str) -> Result<Option<AttemptRecord>> {
        Ok(self
            .attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("Attempt registry lock poisoned"))?
            .get(run_id)
            .cloned())
    }

    async fn publish_attempt_active(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool> {
        anyhow::ensure!(
            !run_id.trim().is_empty() && !attempt_id.trim().is_empty() && lease_epoch > 0,
            "invalid Attempt identity"
        );
        let now = unix_now_ms();
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("Attempt registry lock poisoned"))?;
        let current = attempts.get(run_id);
        if current.is_some_and(|record| {
            record.lease_epoch > lease_epoch
                || (record.lease_epoch == lease_epoch
                    && (record.attempt_id != attempt_id
                        || record.state == AttemptRecordState::Terminal))
        }) {
            return Ok(false);
        }
        let revision = current.map_or(1, |record| record.revision.saturating_add(1));
        attempts.insert(
            run_id.to_owned(),
            AttemptRecord {
                revision,
                run_id: run_id.to_owned(),
                attempt_id: attempt_id.to_owned(),
                lease_epoch,
                state: AttemptRecordState::Active,
                heartbeat_at_unix_ms: now,
                expires_at_unix_ms: now.saturating_add(ttl_ms.max(1)),
                terminal_result: None,
            },
        );
        Ok(true)
    }

    async fn publish_attempt_terminal(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        result: Value,
    ) -> Result<bool> {
        let now = unix_now_ms();
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("Attempt registry lock poisoned"))?;
        let Some(record) = attempts.get_mut(run_id) else {
            return Ok(false);
        };
        if record.attempt_id != attempt_id || record.lease_epoch != lease_epoch {
            return Ok(false);
        }
        if record.state == AttemptRecordState::Terminal {
            return Ok(record.terminal_result.as_ref() == Some(&result));
        }
        record.revision = record.revision.saturating_add(1);
        record.state = AttemptRecordState::Terminal;
        record.heartbeat_at_unix_ms = now;
        record.expires_at_unix_ms = now;
        record.terminal_result = Some(result);
        Ok(true)
    }

    async fn heartbeat_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> Result<bool> {
        let now = unix_now_ms();
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| anyhow::anyhow!("Attempt registry lock poisoned"))?;
        let Some(record) = attempts.get_mut(run_id) else {
            return Ok(false);
        };
        if record.attempt_id != attempt_id
            || record.lease_epoch != lease_epoch
            || record.state != AttemptRecordState::Active
        {
            return Ok(false);
        }
        record.revision = record.revision.saturating_add(1);
        record.heartbeat_at_unix_ms = now;
        record.expires_at_unix_ms = now.saturating_add(ttl_ms.max(1));
        Ok(true)
    }

    async fn append_trajectory(
        &self,
        request: TrajectoryAppendRequest,
    ) -> Result<TrajectoryAppendResponse> {
        let accepted_records = request.records.len();
        Ok(TrajectoryAppendResponse {
            dataset: format!(
                "{}/{}/{}/events.lance",
                request.storage.trim_end_matches('/'),
                request.agent_id,
                request.session_id
            ),
            storage: request.storage,
            agent_id: request.agent_id,
            session_id: request.session_id,
            accepted_records,
            status: "ok".into(),
            note: "in-memory control transport; trajectory was not persisted".into(),
        })
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attempt_heartbeat_renews_only_the_active_fenced_attempt() {
        let control = MemoryChronicleControl::new("memory");
        assert!(control
            .publish_attempt_active("run-1", "attempt-1", 3, 1_000)
            .await
            .unwrap());
        let initial = control.get_attempt("run-1").await.unwrap().unwrap();
        assert!(!control
            .heartbeat_attempt("run-1", "attempt-old", 2, 2_000)
            .await
            .unwrap());
        assert!(control
            .heartbeat_attempt("run-1", "attempt-1", 3, 2_000)
            .await
            .unwrap());
        let renewed = control.get_attempt("run-1").await.unwrap().unwrap();
        assert_eq!(renewed.revision, initial.revision + 1);
        assert!(renewed.expires_at_unix_ms >= initial.expires_at_unix_ms);

        assert!(control
            .publish_attempt_terminal("run-1", "attempt-1", 3, serde_json::json!({"ok": true}))
            .await
            .unwrap());
        assert!(!control
            .heartbeat_attempt("run-1", "attempt-1", 3, 2_000)
            .await
            .unwrap());
    }
}
