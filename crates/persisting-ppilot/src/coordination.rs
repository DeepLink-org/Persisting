//! Durable pPilot ownership, terminal commit, and restart reconciliation.
//!
//! The local result journal deliberately precedes the pChronicle RunCommit:
//! after a crash, the reconciler can replay either the CAS commit or the sink
//! append without executing the workload again.

use crate::digest::sha256_hex;
use crate::sink::{persist_terminal, ResultSink};
use crate::task::TaskResult;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use persisting_agentctl::{
    AttemptId, RunCommitRequest, RunId, RunLeaseRecord, RunResult, RunState,
};
#[cfg(not(test))]
use persisting_pchronicle_client::ChronicleControlProcessClient;
#[cfg(test)]
use persisting_pchronicle_client::MemoryChronicleControl;
use persisting_pchronicle_client::{
    AttemptRecordState, ChronicleControl, CommitRunOutcome, LeaseAcquireOutcome,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

const RESULT_JOURNAL_SCHEMA_VERSION: u32 = 1;
pub const MIN_LEASE_TTL_MS: u64 = 1_000;
static OWNER_SEQUENCE: AtomicU64 = AtomicU64::new(1);

enum LeaseRenewalOutcome {
    Stopped,
    DeadlineExceeded,
    Finished(Result<bool>),
}

async fn wait_for_lease_renewal<F>(
    stop: &CancellationToken,
    deadline: tokio::time::Instant,
    renewal: F,
) -> LeaseRenewalOutcome
where
    F: Future<Output = Result<bool>>,
{
    tokio::select! {
        _ = stop.cancelled() => LeaseRenewalOutcome::Stopped,
        renewal = tokio::time::timeout_at(deadline, renewal) => match renewal {
            Ok(result) => LeaseRenewalOutcome::Finished(result),
            Err(_) => LeaseRenewalOutcome::DeadlineExceeded,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DurableResultStatus {
    Staged,
    Committed,
    Fenced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableResultRecord {
    schema_version: u32,
    task_id: String,
    run_id: RunId,
    attempt_id: AttemptId,
    lease_epoch: u64,
    result_digest: String,
    result: TaskResult,
    status: DurableResultStatus,
    sink_persisted: bool,
}

#[derive(Debug, Clone)]
pub enum AttemptObservation {
    /// No live attempt is visible; the task should receive a new lease.
    Absent,
    /// The runtime still owns an attempt for this Run.
    Active {
        attempt_id: AttemptId,
        lease_epoch: u64,
    },
    /// The runtime has a terminal result that was not yet committed.
    Terminal(Box<TaskResult>),
    /// The durable lease has not expired, but pVisor has not published an
    /// Attempt record yet. Reconciliation must defer rather than re-dispatch.
    Pending,
}

/// Runtime seam used by the reconciler. A future remote pVisor registry can
/// implement this without changing the durable control protocol.
#[async_trait]
pub trait AttemptObserver: Send + Sync {
    async fn observe(&self, lease: &RunLeaseRecord) -> Result<AttemptObservation>;

    async fn cancel_stale(&self, _run_id: &RunId, _attempt_id: &AttemptId) -> Result<()> {
        Ok(())
    }
}

/// Startup observer for today's process-local pVisor workers. Their processes
/// cannot survive a pPilot process restart, so every uncommitted lease is orphaned.
pub struct ProcessLocalAttemptObserver;

#[async_trait]
impl AttemptObserver for ProcessLocalAttemptObserver {
    async fn observe(&self, _lease: &RunLeaseRecord) -> Result<AttemptObservation> {
        Ok(AttemptObservation::Absent)
    }
}

/// pChronicle-backed observer used by production resume/reconciliation.
pub struct DurableAttemptObserver {
    control: Arc<dyn ChronicleControl>,
}

impl DurableAttemptObserver {
    fn new(control: Arc<dyn ChronicleControl>) -> Self {
        Self { control }
    }
}

#[async_trait]
impl AttemptObserver for DurableAttemptObserver {
    async fn observe(&self, lease: &RunLeaseRecord) -> Result<AttemptObservation> {
        let Some(record) = self.control.get_attempt(lease.run_id.as_str()).await? else {
            return Ok(if lease.expires_at_unix_ms > unix_now_ms() {
                AttemptObservation::Pending
            } else {
                AttemptObservation::Absent
            });
        };
        if record.lease_epoch != lease.epoch {
            return Ok(AttemptObservation::Absent);
        }
        match record.state {
            AttemptRecordState::Active if record.is_live_at(unix_now_ms()) => {
                Ok(AttemptObservation::Active {
                    attempt_id: AttemptId::new(record.attempt_id),
                    lease_epoch: record.lease_epoch,
                })
            }
            AttemptRecordState::Active => Ok(AttemptObservation::Absent),
            AttemptRecordState::Terminal => {
                let value = record
                    .terminal_result
                    .context("terminal Attempt record has no result")?;
                let result: RunResult = serde_json::from_value(value)
                    .context("decode terminal pVisor RunResult from Attempt registry")?;
                let task_id = lease
                    .task_id
                    .as_deref()
                    .context("terminal Run lease has no task_id")?;
                Ok(AttemptObservation::Terminal(Box::new(
                    crate::executor::run_result_to_task_result(
                        result,
                        task_id,
                        "recovered",
                        crate::task::unix_now(),
                    ),
                )))
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub committed_task_ids: BTreeSet<String>,
    pub retry_task_ids: BTreeSet<String>,
    pub deferred_task_ids: BTreeSet<String>,
    pub recovered_commits: usize,
    pub recovered_sink_appends: usize,
    pub fenced_results: usize,
    pub active_attempts: usize,
    pub stale_attempts_cancelled: usize,
}

/// Shared pPilot coordination state for one job/sink.
#[derive(Clone)]
pub struct RunCoordinator {
    control: Arc<dyn ChronicleControl>,
    journal_root: PathBuf,
    lease_ttl_ms: u64,
    owner_id: String,
    run_id_prefix: Option<String>,
    orphaned_runs: Arc<Mutex<BTreeSet<RunId>>>,
    heartbeats: Arc<Mutex<BTreeMap<RunId, CancellationToken>>>,
}

impl std::fmt::Debug for RunCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunCoordinator")
            .field("control_root", &self.control.root_uri())
            .field("journal_root", &self.journal_root)
            .field("lease_ttl_ms", &self.lease_ttl_ms)
            .field("owner_id", &self.owner_id)
            .field("run_id_prefix", &self.run_id_prefix)
            .finish()
    }
}

impl RunCoordinator {
    pub async fn open(
        control_root: impl AsRef<str>,
        sink_root: impl Into<PathBuf>,
        lease_ttl_ms: u64,
    ) -> Result<Self> {
        Self::open_with_binary("pchronicle", control_root, sink_root, lease_ttl_ms, None).await
    }

    pub async fn open_for_job(
        control_root: impl AsRef<str>,
        sink_root: impl Into<PathBuf>,
        lease_ttl_ms: u64,
        job_id: &str,
    ) -> Result<Self> {
        Self::open_with_binary(
            "pchronicle",
            control_root,
            sink_root,
            lease_ttl_ms,
            Some(crate::executor::job_run_id_prefix(job_id)),
        )
        .await
    }

    pub async fn open_for_job_with_binary(
        binary: impl AsRef<Path>,
        control_root: impl AsRef<str>,
        sink_root: impl Into<PathBuf>,
        lease_ttl_ms: u64,
        job_id: &str,
    ) -> Result<Self> {
        Self::open_with_binary(
            binary,
            control_root,
            sink_root,
            lease_ttl_ms,
            Some(crate::executor::job_run_id_prefix(job_id)),
        )
        .await
    }

    async fn open_with_binary(
        binary: impl AsRef<Path>,
        control_root: impl AsRef<str>,
        sink_root: impl Into<PathBuf>,
        lease_ttl_ms: u64,
        run_id_prefix: Option<String>,
    ) -> Result<Self> {
        let control_root = control_root.as_ref().to_owned();
        #[cfg(test)]
        let control: Arc<dyn ChronicleControl> = {
            let _ = binary;
            Arc::new(MemoryChronicleControl::new(control_root))
        };
        #[cfg(not(test))]
        let control = Arc::new(
            ChronicleControlProcessClient::spawn(binary, control_root)
                .await
                .context("start pChronicle control client")?,
        );
        Self::open_with_control(control, sink_root, lease_ttl_ms, run_id_prefix).await
    }

    pub async fn open_with_control(
        control: Arc<dyn ChronicleControl>,
        sink_root: impl Into<PathBuf>,
        lease_ttl_ms: u64,
        run_id_prefix: Option<String>,
    ) -> Result<Self> {
        anyhow::ensure!(
            lease_ttl_ms >= MIN_LEASE_TTL_MS,
            "lease TTL must be at least {MIN_LEASE_TTL_MS}ms; got {lease_ttl_ms}ms"
        );
        let journal_root = sink_root.into().join(".ppilot-state").join("results");
        tokio::fs::create_dir_all(&journal_root)
            .await
            .with_context(|| format!("create result journal {}", journal_root.display()))?;
        Ok(Self {
            control,
            journal_root,
            lease_ttl_ms,
            owner_id: unique_owner_id(),
            run_id_prefix,
            orphaned_runs: Arc::new(Mutex::new(BTreeSet::new())),
            heartbeats: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    pub fn control(&self) -> &Arc<dyn ChronicleControl> {
        &self.control
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn durable_attempt_observer(&self) -> DurableAttemptObserver {
        DurableAttemptObserver::new(Arc::clone(&self.control))
    }

    pub fn lease_ttl_ms(&self) -> u64 {
        self.lease_ttl_ms
    }

    pub(crate) fn start_lease_heartbeat(
        &self,
        run_id: RunId,
        lease_epoch: u64,
        task_cancel: CancellationToken,
    ) -> Result<LeaseHeartbeat> {
        let stop = CancellationToken::new();
        let old = self
            .heartbeats
            .lock()
            .map_err(|_| anyhow::anyhow!("lease heartbeat lock poisoned"))?
            .insert(run_id.clone(), stop.clone());
        if let Some(old) = old {
            old.cancel();
        }
        let control = Arc::clone(&self.control);
        let owner = self.owner_id.clone();
        let ttl_ms = self.lease_ttl_ms;
        let heartbeat_stop = stop.clone();
        let heartbeat_run_id = run_id.clone();
        tokio::spawn(async move {
            let interval_ms = (ttl_ms / 3).clamp(1, 10_000);
            let interval = std::time::Duration::from_millis(interval_ms);
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_success = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = heartbeat_stop.cancelled() => break,
                    _ = ticker.tick() => {}
                }
                let deadline = last_success + std::time::Duration::from_millis(ttl_ms);
                let renewal = wait_for_lease_renewal(
                    &heartbeat_stop,
                    deadline,
                    control.renew_lease(&heartbeat_run_id, lease_epoch, &owner, ttl_ms),
                )
                .await;
                match renewal {
                    LeaseRenewalOutcome::Stopped => break,
                    LeaseRenewalOutcome::DeadlineExceeded => {
                        tracing::warn!(
                            run_id = %heartbeat_run_id,
                            lease_epoch,
                            "Run lease renewal exceeded its validity deadline"
                        );
                        task_cancel.cancel();
                        break;
                    }
                    LeaseRenewalOutcome::Finished(Ok(true)) => {
                        last_success = tokio::time::Instant::now()
                    }
                    LeaseRenewalOutcome::Finished(Ok(false)) => {
                        tracing::warn!(run_id = %heartbeat_run_id, lease_epoch, "Run lease ownership lost");
                        task_cancel.cancel();
                        break;
                    }
                    LeaseRenewalOutcome::Finished(Err(error)) => {
                        tracing::warn!(run_id = %heartbeat_run_id, lease_epoch, %error, "Run lease renewal failed");
                        if last_success.elapsed() >= std::time::Duration::from_millis(ttl_ms) {
                            task_cancel.cancel();
                            break;
                        }
                    }
                }
            }
        });
        Ok(LeaseHeartbeat {
            run_id,
            stop,
            heartbeats: Arc::clone(&self.heartbeats),
            detached: false,
        })
    }

    /// Acquire a monotonically increasing fencing token. A committed Run is
    /// never leased again.
    pub async fn acquire_lease(&self, run_id: &RunId, task_id: &str, owner: &str) -> Result<u64> {
        let force = self
            .orphaned_runs
            .lock()
            .map_err(|_| anyhow::anyhow!("orphaned Run set lock poisoned"))?
            .remove(run_id);
        let outcome = if force {
            self.control
                .takeover_lease(run_id, Some(task_id), owner, self.lease_ttl_ms)
                .await?
        } else {
            self.control
                .acquire_lease(run_id, Some(task_id), owner, self.lease_ttl_ms)
                .await?
        };
        match outcome {
            LeaseAcquireOutcome::Acquired(lease) => Ok(lease.epoch),
            LeaseAcquireOutcome::Held(lease) => bail!(
                "Run {run_id} lease epoch {} is held by {} until {}",
                lease.epoch,
                lease.owner,
                lease.expires_at_unix_ms
            ),
            LeaseAcquireOutcome::AlreadyCommitted(_) => {
                bail!("Run {run_id} is already committed")
            }
        }
    }

    /// Stage, fence, commit, then publish a terminal result to the user sink.
    pub async fn finalize_result(&self, sink: &dyn ResultSink, result: &TaskResult) -> Result<()> {
        let run_id = result.run_id.as_deref().map(RunId::new);
        let outcome = async {
            let mut record = self.stage_result(result).await?;
            self.commit_record(&mut record).await?;
            persist_terminal(sink, &record.result).await?;
            record.sink_persisted = true;
            self.write_record(&record).await
        }
        .await;
        if let Some(run_id) = run_id {
            self.stop_lease_heartbeat(&run_id);
        }
        outcome
    }

    /// Compare durable records, pChronicle control state, and observable pVisor
    /// attempts. It never guesses success: only a terminal payload can create a commit.
    pub async fn reconcile(
        &self,
        sink: &dyn ResultSink,
        observer: &dyn AttemptObserver,
    ) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();

        for mut record in self.read_records().await? {
            if self
                .run_id_prefix
                .as_ref()
                .is_some_and(|prefix| !record.run_id.as_str().starts_with(prefix))
            {
                continue;
            }
            if record.schema_version != RESULT_JOURNAL_SCHEMA_VERSION {
                bail!(
                    "unsupported pPilot result journal schema {} for task {}",
                    record.schema_version,
                    record.task_id
                );
            }
            match record.status {
                DurableResultStatus::Staged => match self.commit_record(&mut record).await {
                    Ok(()) => report.recovered_commits += 1,
                    Err(error) => {
                        tracing::warn!(task_id = %record.task_id, %error, "fencing staged result during reconciliation");
                        record.status = DurableResultStatus::Fenced;
                        self.write_record(&record).await?;
                        report.fenced_results += 1;
                        report.retry_task_ids.insert(record.task_id.clone());
                        self.note_orphaned(&record.run_id)?;
                        continue;
                    }
                },
                DurableResultStatus::Fenced => {
                    report.fenced_results += 1;
                    report.retry_task_ids.insert(record.task_id.clone());
                    self.note_orphaned(&record.run_id)?;
                    continue;
                }
                DurableResultStatus::Committed => {}
            }
            report.committed_task_ids.insert(record.task_id.clone());
            if !record.sink_persisted {
                persist_terminal(sink, &record.result).await?;
                record.sink_persisted = true;
                self.write_record(&record).await?;
                report.recovered_sink_appends += 1;
            }
        }

        for control in self.control.list_runs().await? {
            if self
                .run_id_prefix
                .as_ref()
                .is_some_and(|prefix| !control.run_id.as_str().starts_with(prefix))
            {
                continue;
            }
            if let Some(commit) = control.commit {
                if let Some(task_id) = commit.request.task_id {
                    report.committed_task_ids.insert(task_id);
                }
                continue;
            }
            let Some(lease) = control.lease else {
                continue;
            };
            let Some(task_id) = lease.task_id.clone() else {
                continue;
            };
            if report.committed_task_ids.contains(&task_id) {
                continue;
            }
            match observer.observe(&lease).await? {
                AttemptObservation::Absent => {
                    self.note_orphaned(&lease.run_id)?;
                    report.retry_task_ids.insert(task_id);
                }
                AttemptObservation::Pending => {
                    report.active_attempts += 1;
                    report.deferred_task_ids.insert(task_id);
                }
                AttemptObservation::Active {
                    attempt_id,
                    lease_epoch,
                } if lease_epoch == lease.epoch => {
                    report.active_attempts += 1;
                    report.deferred_task_ids.insert(task_id.clone());
                    // Backfill attempt identity if the submit succeeded before a crash.
                    let _ = self
                        .control
                        .bind_attempt(&lease.run_id, lease.epoch, attempt_id)
                        .await?;
                }
                AttemptObservation::Active { attempt_id, .. } => {
                    observer.cancel_stale(&lease.run_id, &attempt_id).await?;
                    report.stale_attempts_cancelled += 1;
                    self.note_orphaned(&lease.run_id)?;
                    report.retry_task_ids.insert(task_id);
                }
                AttemptObservation::Terminal(result) => {
                    self.finalize_result(sink, &result).await?;
                    report.recovered_commits += 1;
                    report.recovered_sink_appends += 1;
                    report.committed_task_ids.insert(task_id);
                }
            }
        }
        for task_id in &report.committed_task_ids {
            report.retry_task_ids.remove(task_id);
        }
        Ok(report)
    }

    async fn stage_result(&self, result: &TaskResult) -> Result<DurableResultRecord> {
        let run_id = result
            .run_id
            .as_deref()
            .context("terminal TaskResult has no run_id")?;
        let attempt_id = result
            .attempt_id
            .as_deref()
            .context("terminal TaskResult has no attempt_id")?;
        if result.lease_epoch == 0 {
            bail!("terminal TaskResult has no lease epoch");
        }
        let record = DurableResultRecord {
            schema_version: RESULT_JOURNAL_SCHEMA_VERSION,
            task_id: result.task_id.clone(),
            run_id: RunId::new(run_id),
            attempt_id: AttemptId::new(attempt_id),
            lease_epoch: result.lease_epoch,
            result_digest: result_digest(result)?,
            result: result.clone(),
            status: DurableResultStatus::Staged,
            sink_persisted: false,
        };
        self.write_record(&record).await?;
        Ok(record)
    }

    async fn commit_record(&self, record: &mut DurableResultRecord) -> Result<()> {
        let bound = self
            .control
            .bind_attempt(
                &record.run_id,
                record.lease_epoch,
                record.attempt_id.clone(),
            )
            .await?;
        if !bound {
            record.status = DurableResultStatus::Fenced;
            self.write_record(record).await?;
            bail!(
                "Run result attempt {} no longer owns lease epoch {}",
                record.attempt_id,
                record.lease_epoch
            );
        }
        let outcome = self
            .control
            .commit_run(RunCommitRequest {
                run_id: record.run_id.clone(),
                task_id: Some(record.task_id.clone()),
                attempt_id: record.attempt_id.clone(),
                lease_epoch: record.lease_epoch,
                state: task_result_state(&record.result),
                event_high_watermark: None,
                result_digest: record.result_digest.clone(),
            })
            .await?;
        match outcome {
            CommitRunOutcome::Committed(_) | CommitRunOutcome::AlreadyCommitted(_) => {
                record.status = DurableResultStatus::Committed;
                self.write_record(record).await
            }
            CommitRunOutcome::StaleLease {
                supplied_epoch,
                current_epoch,
            } => {
                record.status = DurableResultStatus::Fenced;
                self.write_record(record).await?;
                bail!(
                    "stale Run result fenced: supplied epoch {supplied_epoch}, current {current_epoch:?}"
                )
            }
            CommitRunOutcome::Conflict(existing) => {
                record.status = DurableResultStatus::Fenced;
                self.write_record(record).await?;
                bail!(
                    "RunCommit conflicts with attempt {} epoch {}",
                    existing.request.attempt_id,
                    existing.request.lease_epoch
                )
            }
        }
    }

    async fn read_records(&self) -> Result<Vec<DurableResultRecord>> {
        let root = self.journal_root.clone();
        tokio::task::spawn_blocking(move || {
            let mut records = Vec::new();
            for entry in std::fs::read_dir(&root)? {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read result journal {}", path.display()))?;
                records.push(
                    serde_json::from_slice(&bytes)
                        .with_context(|| format!("decode result journal {}", path.display()))?,
                );
            }
            Ok(records)
        })
        .await?
    }

    async fn write_record(&self, record: &DurableResultRecord) -> Result<()> {
        let path = self.record_path(&record.task_id);
        let bytes = serde_json::to_vec_pretty(record)?;
        tokio::task::spawn_blocking(move || atomic_write(&path, &bytes)).await?
    }

    fn record_path(&self, task_id: &str) -> PathBuf {
        let name = format!("{}.json", sha256_hex(task_id));
        self.journal_root.join(name)
    }

    fn note_orphaned(&self, run_id: &RunId) -> Result<()> {
        self.orphaned_runs
            .lock()
            .map_err(|_| anyhow::anyhow!("orphaned Run set lock poisoned"))?
            .insert(run_id.clone());
        Ok(())
    }

    fn stop_lease_heartbeat(&self, run_id: &RunId) {
        if let Ok(mut heartbeats) = self.heartbeats.lock() {
            if let Some(stop) = heartbeats.remove(run_id) {
                stop.cancel();
            }
        }
    }
}

pub(crate) struct LeaseHeartbeat {
    run_id: RunId,
    stop: CancellationToken,
    heartbeats: Arc<Mutex<BTreeMap<RunId, CancellationToken>>>,
    detached: bool,
}

impl LeaseHeartbeat {
    pub(crate) fn detach(mut self) {
        self.detached = true;
    }
}

impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        self.stop.cancel();
        if let Ok(mut heartbeats) = self.heartbeats.lock() {
            heartbeats.remove(&self.run_id);
        }
    }
}

fn task_result_state(result: &TaskResult) -> RunState {
    if result.ok && !result.cancelled {
        RunState::Completed
    } else if result.cancelled {
        RunState::Cancelled
    } else {
        RunState::Failed
    }
}

fn unique_owner_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = OWNER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("ppilot:{}:{nanos}:{sequence}", std::process::id())
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn result_digest(result: &TaskResult) -> Result<String> {
    // serde_json's default map representation is key-sorted; hashing the Value
    // makes HashMap insertion order irrelevant across a crash/reload boundary.
    let canonical = serde_json::to_value(result)?;
    Ok(format!(
        "sha256:{}",
        sha256_hex(serde_json::to_vec(&canonical)?)
    ))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("journal path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("result"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sink::JsonlFileSink;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn terminal(task: &str, run: &str, attempt: &str, epoch: u64) -> TaskResult {
        let mut result = TaskResult::success(task, json!({"answer": 42}), "w0", 1.0);
        result.run_id = Some(run.into());
        result.attempt_id = Some(attempt.into());
        result.lease_epoch = epoch;
        result
    }

    #[tokio::test]
    async fn durable_observer_defers_live_attempt_and_recovers_terminal_result() {
        let dir = tempfile::tempdir().unwrap();
        let coordinator = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            30_000,
        )
        .await
        .unwrap();
        let run = RunId::new("run-durable-observer");
        let epoch = coordinator
            .acquire_lease(&run, "task-durable", "driver-a")
            .await
            .unwrap();
        let lease = coordinator
            .control
            .get_run(&run)
            .await
            .unwrap()
            .unwrap()
            .lease
            .unwrap();
        let observer = coordinator.durable_attempt_observer();
        assert!(matches!(
            observer.observe(&lease).await.unwrap(),
            AttemptObservation::Pending
        ));

        coordinator
            .control
            .publish_attempt_active(run.as_str(), "attempt-durable", epoch, 30_000)
            .await
            .unwrap();
        assert!(matches!(
            observer.observe(&lease).await.unwrap(),
            AttemptObservation::Active { lease_epoch, .. } if lease_epoch == epoch
        ));

        let mut spec =
            persisting_agentctl::RunSpec::process(run.as_str(), "ppilot", "ppilot-plan-host");
        spec.lease_epoch = epoch;
        let run_result = crate::executor::task_result_to_run_result(
            spec,
            AttemptId::new("attempt-durable"),
            terminal("task-durable", run.as_str(), "attempt-durable", epoch),
        );
        coordinator
            .control
            .publish_attempt_terminal(
                run.as_str(),
                "attempt-durable",
                epoch,
                serde_json::to_value(run_result).unwrap(),
            )
            .await
            .unwrap();
        let AttemptObservation::Terminal(recovered) = observer.observe(&lease).await.unwrap()
        else {
            panic!("terminal Attempt should be recoverable");
        };
        assert_eq!(recovered.task_id, "task-durable");
        assert_eq!(recovered.attempt_id.as_deref(), Some("attempt-durable"));
        assert_eq!(recovered.lease_epoch, epoch);
    }

    #[tokio::test]
    async fn reconciliation_closes_stage_before_commit_crash_window() {
        let dir = tempfile::tempdir().unwrap();
        let coordinator = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            30_000,
        )
        .await
        .unwrap();
        let run = RunId::new("run-1");
        let epoch = coordinator
            .acquire_lease(&run, "task-1", "driver-a")
            .await
            .unwrap();
        coordinator
            .stage_result(&terminal("task-1", "run-1", "attempt-1", epoch))
            .await
            .unwrap();

        let sink = JsonlFileSink::open(dir.path().join("sink")).await.unwrap();
        let report = coordinator
            .reconcile(&sink, &ProcessLocalAttemptObserver)
            .await
            .unwrap();
        assert_eq!(report.recovered_commits, 1);
        assert_eq!(report.recovered_sink_appends, 1);
        assert!(report.committed_task_ids.contains("task-1"));
        assert!(coordinator
            .control
            .get_run(&run)
            .await
            .unwrap()
            .unwrap()
            .commit
            .is_some());
    }

    #[tokio::test]
    async fn reconciliation_closes_commit_before_sink_crash_window() {
        let dir = tempfile::tempdir().unwrap();
        let sink_root = dir.path().join("sink");
        let coordinator = RunCoordinator::open(dir.path().to_string_lossy(), &sink_root, 30_000)
            .await
            .unwrap();
        let run = RunId::new("run-after-commit");
        let epoch = coordinator
            .acquire_lease(&run, "task-after-commit", "driver-a")
            .await
            .unwrap();
        let mut record = coordinator
            .stage_result(&terminal(
                "task-after-commit",
                "run-after-commit",
                "attempt-after-commit",
                epoch,
            ))
            .await
            .unwrap();
        coordinator.commit_record(&mut record).await.unwrap();

        let sink = JsonlFileSink::open(&sink_root).await.unwrap();
        let report = coordinator
            .reconcile(&sink, &ProcessLocalAttemptObserver)
            .await
            .unwrap();
        assert_eq!(report.recovered_commits, 0);
        assert_eq!(report.recovered_sink_appends, 1);
        let ready = tokio::fs::read_to_string(sink_root.join("ready.ndjson"))
            .await
            .unwrap();
        assert!(ready.contains("task-after-commit"));
    }

    struct MixedObserver {
        cancelled: AtomicUsize,
    }

    #[async_trait]
    impl AttemptObserver for MixedObserver {
        async fn observe(&self, lease: &RunLeaseRecord) -> Result<AttemptObservation> {
            if lease.task_id.as_deref() == Some("active") {
                Ok(AttemptObservation::Active {
                    attempt_id: AttemptId::new("attempt-active"),
                    lease_epoch: lease.epoch,
                })
            } else {
                Ok(AttemptObservation::Active {
                    attempt_id: AttemptId::new("attempt-stale"),
                    lease_epoch: lease.epoch.saturating_sub(1),
                })
            }
        }

        async fn cancel_stale(&self, _run_id: &RunId, _attempt_id: &AttemptId) -> Result<()> {
            self.cancelled.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[tokio::test]
    async fn reconciler_keeps_current_attempt_and_cancels_stale_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let coordinator = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            30_000,
        )
        .await
        .unwrap();
        coordinator
            .acquire_lease(&RunId::new("run-active"), "active", "driver-a")
            .await
            .unwrap();
        coordinator
            .acquire_lease(&RunId::new("run-stale"), "stale", "driver-a")
            .await
            .unwrap();
        let observer = MixedObserver {
            cancelled: AtomicUsize::new(0),
        };
        let sink = JsonlFileSink::open(dir.path().join("sink")).await.unwrap();
        let report = coordinator.reconcile(&sink, &observer).await.unwrap();
        assert_eq!(report.active_attempts, 1);
        assert_eq!(report.stale_attempts_cancelled, 1);
        assert_eq!(observer.cancelled.load(Ordering::Acquire), 1);
        assert!(!report.retry_task_ids.contains("active"));
        assert!(report.retry_task_ids.contains("stale"));
    }

    #[tokio::test]
    async fn job_scoped_reconciler_ignores_other_jobs_in_shared_control_root() {
        let dir = tempfile::tempdir().unwrap();
        let sink_root = dir.path().join("sink");
        let shared: Arc<dyn ChronicleControl> =
            Arc::new(MemoryChronicleControl::new(dir.path().to_string_lossy()));
        let unscoped =
            RunCoordinator::open_with_control(Arc::clone(&shared), &sink_root, 30_000, None)
                .await
                .unwrap();
        let other_run = RunId::new("ppilot-job-b-task-1");
        let epoch = unscoped
            .acquire_lease(&other_run, "same-task-id", "driver-b")
            .await
            .unwrap();
        unscoped
            .stage_result(&terminal(
                "same-task-id",
                other_run.as_str(),
                "attempt-b",
                epoch,
            ))
            .await
            .unwrap();

        let scoped = RunCoordinator::open_with_control(
            shared,
            &sink_root,
            30_000,
            Some(crate::executor::job_run_id_prefix("job-a")),
        )
        .await
        .unwrap();
        let sink = JsonlFileSink::open(&sink_root).await.unwrap();
        let report = scoped
            .reconcile(&sink, &ProcessLocalAttemptObserver)
            .await
            .unwrap();
        assert!(report.committed_task_ids.is_empty());
        assert!(report.retry_task_ids.is_empty());
        assert!(scoped
            .control
            .get_run(&other_run)
            .await
            .unwrap()
            .unwrap()
            .commit
            .is_none());
    }

    #[tokio::test]
    async fn renewal_wait_honors_the_lease_deadline() {
        let stop = CancellationToken::new();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            wait_for_lease_renewal(
                &stop,
                tokio::time::Instant::now() + std::time::Duration::from_millis(10),
                std::future::pending(),
            ),
        )
        .await
        .expect("renewal deadline should not hang");

        assert!(matches!(outcome, LeaseRenewalOutcome::DeadlineExceeded));
    }

    #[tokio::test]
    async fn lease_ttl_below_minimum_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let error = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            MIN_LEASE_TTL_MS - 1,
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("lease TTL must be at least 1000ms"));
    }

    #[tokio::test]
    async fn heartbeat_keeps_long_attempt_lease_unexpired() {
        let dir = tempfile::tempdir().unwrap();
        let coordinator = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            MIN_LEASE_TTL_MS,
        )
        .await
        .unwrap();
        let run = RunId::new("run-heartbeat");
        let epoch = coordinator
            .acquire_lease(&run, "task-heartbeat", coordinator.owner_id())
            .await
            .unwrap();
        let initial_expiry = coordinator
            .control
            .get_run(&run)
            .await
            .unwrap()
            .unwrap()
            .lease
            .unwrap()
            .expires_at_unix_ms;
        let heartbeat = coordinator
            .start_lease_heartbeat(run.clone(), epoch, CancellationToken::new())
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let expiry = coordinator
                    .control
                    .get_run(&run)
                    .await
                    .unwrap()
                    .unwrap()
                    .lease
                    .unwrap()
                    .expires_at_unix_ms;
                if expiry > initial_expiry {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("heartbeat should durably renew the lease");
        assert!(matches!(
            coordinator
                .control
                .acquire_lease(
                    &run,
                    Some("task-heartbeat"),
                    "competitor",
                    MIN_LEASE_TTL_MS,
                )
                .await
                .unwrap(),
            LeaseAcquireOutcome::Held(held) if held.epoch == epoch
        ));
        drop(heartbeat);
    }

    #[tokio::test]
    async fn newer_epoch_fences_a_staged_old_result() {
        let dir = tempfile::tempdir().unwrap();
        let coordinator = RunCoordinator::open(
            dir.path().to_string_lossy(),
            dir.path().join("sink"),
            30_000,
        )
        .await
        .unwrap();
        let run = RunId::new("run-2");
        let old = coordinator
            .acquire_lease(&run, "task-2", "driver-a")
            .await
            .unwrap();
        coordinator
            .stage_result(&terminal("task-2", "run-2", "attempt-old", old))
            .await
            .unwrap();
        let new = coordinator
            .control
            .takeover_lease(&run, Some("task-2"), "driver-b", 30_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(new) = new else {
            panic!("takeover should acquire")
        };
        assert!(new.epoch > old);

        let sink = JsonlFileSink::open(dir.path().join("sink")).await.unwrap();
        let report = coordinator
            .reconcile(&sink, &ProcessLocalAttemptObserver)
            .await
            .unwrap();
        assert_eq!(report.fenced_results, 1);
        assert!(report.retry_task_ids.contains("task-2"));
        assert!(coordinator
            .control
            .get_run(&run)
            .await
            .unwrap()
            .unwrap()
            .commit
            .is_none());
    }

    #[tokio::test]
    async fn committed_replay_is_idempotent_in_sink_and_cas() {
        let dir = tempfile::tempdir().unwrap();
        let sink_root = dir.path().join("sink");
        let coordinator = RunCoordinator::open(dir.path().to_string_lossy(), &sink_root, 30_000)
            .await
            .unwrap();
        let run = RunId::new("run-3");
        let epoch = coordinator
            .acquire_lease(&run, "task-3", "driver-a")
            .await
            .unwrap();
        let result = terminal("task-3", "run-3", "attempt-3", epoch);
        let sink = JsonlFileSink::open(&sink_root).await.unwrap();
        coordinator.finalize_result(&sink, &result).await.unwrap();
        coordinator
            .reconcile(&sink, &ProcessLocalAttemptObserver)
            .await
            .unwrap();
        let ready = tokio::fs::read_to_string(sink_root.join("ready.ndjson"))
            .await
            .unwrap();
        assert_eq!(ready.lines().count(), 1);
    }
}
