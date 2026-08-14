//! CAS-managed Run leases and terminal commits.
//!
//! Lease acquisition and RunCommit share one per-Run control object. This is
//! important: checking a lease in one object and creating a commit in another
//! leaves a race where a newer lease can be issued between the two operations.

use anyhow::{bail, Context};
use fs2::FileExt;
use futures::TryStreamExt;
use lance::io::ObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectStoreError, ObjectStoreExt, PutMode, UpdateVersion};
use persisting_agentctl::{
    AttemptId, RunCommit, RunCommitRequest, RunControlRecord, RunId, RunLeaseRecord,
    RUN_CONTROL_SCHEMA_VERSION,
};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CONTROL_DIR: &str = "run-control";
const CAS_RETRIES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquireOutcome {
    Acquired(RunLeaseRecord),
    /// Another owner still holds an unexpired lease.
    Held(RunLeaseRecord),
    AlreadyCommitted(RunCommit),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitRunOutcome {
    Committed(RunCommit),
    AlreadyCommitted(RunCommit),
    StaleLease {
        supplied_epoch: u64,
        current_epoch: Option<u64>,
    },
    Conflict(RunCommit),
}

enum Backend {
    Local(PathBuf),
    Object {
        store: Arc<ObjectStore>,
        root: ObjectPath,
    },
}

/// A lightweight pChronicle control store backed by a local directory or an
/// object-store URI. Local updates use an advisory file lock plus atomic rename;
/// object stores use conditional create/update with ETag/version preconditions.
pub struct RunControlStore {
    root_uri: String,
    backend: Backend,
}

impl std::fmt::Debug for RunControlStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunControlStore")
            .field("root_uri", &self.root_uri)
            .finish_non_exhaustive()
    }
}

impl RunControlStore {
    pub async fn open(root: impl AsRef<str>) -> anyhow::Result<Self> {
        let root = root.as_ref().trim();
        if root.is_empty() {
            bail!("Run control root must not be empty");
        }
        if !root.contains("://") {
            let path = PathBuf::from(root).join(CONTROL_DIR);
            tokio::fs::create_dir_all(&path)
                .await
                .with_context(|| format!("create Run control root {}", path.display()))?;
            return Ok(Self {
                root_uri: root.to_string(),
                backend: Backend::Local(path),
            });
        }
        let (store, object_root) = ObjectStore::from_uri(root)
            .await
            .with_context(|| format!("open Run control object store {root}"))?;
        Ok(Self {
            root_uri: root.to_string(),
            backend: Backend::Object {
                store,
                root: object_root.join(CONTROL_DIR),
            },
        })
    }

    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    pub async fn acquire_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> anyhow::Result<LeaseAcquireOutcome> {
        self.acquire_lease_inner(run_id, task_id, owner, ttl_ms, false)
            .await
    }

    /// Replace a lease only after the caller has established that its attempt
    /// is absent or stale (for example through pPilot reconciliation).
    pub async fn takeover_lease(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
    ) -> anyhow::Result<LeaseAcquireOutcome> {
        self.acquire_lease_inner(run_id, task_id, owner, ttl_ms, true)
            .await
    }

    async fn acquire_lease_inner(
        &self,
        run_id: &RunId,
        task_id: Option<&str>,
        owner: &str,
        ttl_ms: u64,
        force: bool,
    ) -> anyhow::Result<LeaseAcquireOutcome> {
        if owner.trim().is_empty() {
            bail!("Run lease owner must not be empty");
        }
        let now = unix_now_ms();
        let owned_run_id = run_id.clone();
        let owned_task_id = task_id.map(str::to_owned);
        let owned_owner = owner.to_string();
        let mutate = move |current: Option<&RunControlRecord>| -> anyhow::Result<Mutation<_>> {
            if let Some(commit) = current.and_then(|record| record.commit.clone()) {
                return Ok(Mutation::Unchanged(LeaseAcquireOutcome::AlreadyCommitted(
                    commit,
                )));
            }
            if let Some(existing) = current.and_then(|record| record.lease.as_ref()) {
                if existing.owner != owned_owner && existing.expires_at_unix_ms > now && !force {
                    return Ok(Mutation::Unchanged(LeaseAcquireOutcome::Held(
                        existing.clone(),
                    )));
                }
                if existing.owner == owned_owner && !force {
                    let mut renewed = existing.clone();
                    renewed.expires_at_unix_ms = now.saturating_add(ttl_ms.max(1));
                    let record = next_record(current, owned_run_id.clone(), |record| {
                        record.lease = Some(renewed.clone());
                    })?;
                    return Ok(Mutation::Replace(
                        Box::new(record),
                        LeaseAcquireOutcome::Acquired(renewed),
                    ));
                }
            }
            let epoch = current
                .and_then(|record| record.lease.as_ref().map(|lease| lease.epoch))
                .unwrap_or(0)
                .checked_add(1)
                .context("Run lease epoch overflow")?;
            let lease = RunLeaseRecord {
                run_id: owned_run_id.clone(),
                task_id: owned_task_id.clone(),
                epoch,
                owner: owned_owner.clone(),
                issued_at_unix_ms: now,
                expires_at_unix_ms: now.saturating_add(ttl_ms.max(1)),
                attempt_id: None,
            };
            let record = next_record(current, owned_run_id.clone(), |record| {
                record.lease = Some(lease.clone());
            })?;
            Ok(Mutation::Replace(
                Box::new(record),
                LeaseAcquireOutcome::Acquired(lease),
            ))
        };
        self.mutate(run_id, mutate).await
    }

    pub async fn bind_attempt(
        &self,
        run_id: &RunId,
        epoch: u64,
        attempt_id: AttemptId,
    ) -> anyhow::Result<bool> {
        let owned_run_id = run_id.clone();
        self.mutate(run_id, move |current| {
            let Some(current) = current else {
                return Ok(Mutation::Unchanged(false));
            };
            if current.commit.is_some()
                || current.lease.as_ref().map(|lease| lease.epoch) != Some(epoch)
            {
                return Ok(Mutation::Unchanged(false));
            }
            if current
                .lease
                .as_ref()
                .and_then(|lease| lease.attempt_id.as_ref())
                == Some(&attempt_id)
            {
                return Ok(Mutation::Unchanged(true));
            }
            let record = next_record(Some(current), owned_run_id.clone(), |record| {
                if let Some(lease) = record.lease.as_mut() {
                    lease.attempt_id = Some(attempt_id.clone());
                }
            })?;
            Ok(Mutation::Replace(Box::new(record), true))
        })
        .await
    }

    /// Extend a lease without changing its fencing epoch. Returns `false` if
    /// ownership has moved or the Run is already committed.
    pub async fn renew_lease(
        &self,
        run_id: &RunId,
        epoch: u64,
        owner: &str,
        ttl_ms: u64,
    ) -> anyhow::Result<bool> {
        let owned_run_id = run_id.clone();
        let owned_owner = owner.to_string();
        self.mutate(run_id, move |current| {
            let Some(current) = current else {
                return Ok(Mutation::Unchanged(false));
            };
            let now = unix_now_ms();
            let matches = current.commit.is_none()
                && current.lease.as_ref().is_some_and(|lease| {
                    lease.epoch == epoch
                        && lease.owner == owned_owner
                        && lease.expires_at_unix_ms > now
                });
            if !matches {
                return Ok(Mutation::Unchanged(false));
            }
            let expires_at = now.saturating_add(ttl_ms.max(1));
            let record = next_record(Some(current), owned_run_id.clone(), |record| {
                if let Some(lease) = record.lease.as_mut() {
                    lease.expires_at_unix_ms = expires_at;
                }
            })?;
            Ok(Mutation::Replace(Box::new(record), true))
        })
        .await
    }

    pub async fn commit_run(&self, request: RunCommitRequest) -> anyhow::Result<CommitRunOutcome> {
        if !request.state.is_terminal() {
            bail!("RunCommit requires a terminal Run state");
        }
        if request.lease_epoch == 0 {
            bail!("RunCommit requires a non-zero lease epoch");
        }
        if request.result_digest.trim().is_empty() {
            bail!("RunCommit result_digest must not be empty");
        }
        let run_id = request.run_id.clone();
        let closure_run_id = run_id.clone();
        self.mutate(&run_id, move |current| {
            if let Some(existing) = current.and_then(|record| record.commit.clone()) {
                if existing.request == request {
                    return Ok(Mutation::Unchanged(CommitRunOutcome::AlreadyCommitted(
                        existing,
                    )));
                }
                return Ok(Mutation::Unchanged(CommitRunOutcome::Conflict(existing)));
            }
            let current_epoch =
                current.and_then(|record| record.lease.as_ref().map(|lease| lease.epoch));
            if current_epoch != Some(request.lease_epoch) {
                return Ok(Mutation::Unchanged(CommitRunOutcome::StaleLease {
                    supplied_epoch: request.lease_epoch,
                    current_epoch,
                }));
            }
            let commit = RunCommit {
                request: request.clone(),
                committed_at_unix_ms: unix_now_ms(),
            };
            let record = next_record(current, closure_run_id.clone(), |record| {
                record.commit = Some(commit.clone());
            })?;
            Ok(Mutation::Replace(
                Box::new(record),
                CommitRunOutcome::Committed(commit),
            ))
        })
        .await
    }

    pub async fn get(&self, run_id: &RunId) -> anyhow::Result<Option<RunControlRecord>> {
        match &self.backend {
            Backend::Local(root) => read_local_record(&record_path(root, run_id)),
            Backend::Object { store, root } => {
                Ok(read_object_record(store, &object_record_path(root, run_id))
                    .await?
                    .map(|(record, _)| record))
            }
        }
    }

    pub async fn list(&self) -> anyhow::Result<Vec<RunControlRecord>> {
        let mut records = match &self.backend {
            Backend::Local(root) => {
                let root = root.clone();
                tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<_>> {
                    let mut out = Vec::new();
                    for entry in std::fs::read_dir(&root)? {
                        let entry = entry?;
                        if entry.path().extension().and_then(|value| value.to_str()) != Some("json")
                        {
                            continue;
                        }
                        if let Some(record) = read_local_record(&entry.path())? {
                            out.push(record);
                        }
                    }
                    Ok(out)
                })
                .await??
            }
            Backend::Object { store, root } => {
                let prefix = root.clone();
                let objects = store
                    .inner
                    .list(Some(&prefix))
                    .try_collect::<Vec<_>>()
                    .await?;
                let mut out = Vec::new();
                for object in objects {
                    if object.location.extension() != Some("json") {
                        continue;
                    }
                    if let Some((record, _)) = read_object_record(store, &object.location).await? {
                        out.push(record);
                    }
                }
                out
            }
        };
        records.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(records)
    }

    async fn mutate<T, F>(&self, run_id: &RunId, mutate: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: Fn(Option<&RunControlRecord>) -> anyhow::Result<Mutation<T>> + Send + Sync + 'static,
    {
        match &self.backend {
            Backend::Local(root) => {
                let path = record_path(root, run_id);
                let lock_path = path.with_extension("lock");
                tokio::task::spawn_blocking(move || {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let lock = OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&lock_path)?;
                    lock.lock_exclusive()?;
                    let current = read_local_record(&path)?;
                    let outcome = mutate(current.as_ref())?;
                    let result = match outcome {
                        Mutation::Unchanged(value) => value,
                        Mutation::Replace(record, value) => {
                            write_local_record(&path, &record)?;
                            value
                        }
                    };
                    FileExt::unlock(&lock)?;
                    Ok(result)
                })
                .await?
            }
            Backend::Object { store, root } => {
                let path = object_record_path(root, run_id);
                for _ in 0..CAS_RETRIES {
                    let current = read_object_record(store, &path).await?;
                    let outcome = mutate(current.as_ref().map(|(record, _)| record))?;
                    let (record, value) = match outcome {
                        Mutation::Unchanged(value) => return Ok(value),
                        Mutation::Replace(record, value) => (record, value),
                    };
                    let mode = match current {
                        None => PutMode::Create,
                        Some((_, version)) => PutMode::Update(version),
                    };
                    let bytes = serde_json::to_vec_pretty(&record)?;
                    match store.inner.put_opts(&path, bytes.into(), mode.into()).await {
                        Ok(_) => return Ok(value),
                        Err(ObjectStoreError::AlreadyExists { .. })
                        | Err(ObjectStoreError::Precondition { .. }) => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                bail!("Run control CAS contention exceeded {CAS_RETRIES} retries")
            }
        }
    }
}

enum Mutation<T> {
    Unchanged(T),
    Replace(Box<RunControlRecord>, T),
}

fn next_record(
    current: Option<&RunControlRecord>,
    run_id: RunId,
    apply: impl FnOnce(&mut RunControlRecord),
) -> anyhow::Result<RunControlRecord> {
    let mut record = current.cloned().unwrap_or(RunControlRecord {
        schema_version: RUN_CONTROL_SCHEMA_VERSION,
        revision: 0,
        run_id: run_id.clone(),
        lease: None,
        commit: None,
    });
    if record.schema_version != RUN_CONTROL_SCHEMA_VERSION || record.run_id != run_id {
        bail!("invalid Run control record for {run_id}");
    }
    record.revision = record
        .revision
        .checked_add(1)
        .context("revision overflow")?;
    apply(&mut record);
    Ok(record)
}

fn encoded_run_id(run_id: &RunId) -> String {
    let mut encoded = String::with_capacity(run_id.as_str().len());
    for byte in run_id.as_str().bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("~{other:02x}")),
        }
    }
    encoded
}

fn record_path(root: &Path, run_id: &RunId) -> PathBuf {
    root.join(format!("{}.json", encoded_run_id(run_id)))
}

fn object_record_path(root: &ObjectPath, run_id: &RunId) -> ObjectPath {
    root.clone()
        .join(format!("{}.json", encoded_run_id(run_id)))
}

fn read_local_record(path: &Path) -> anyhow::Result<Option<RunControlRecord>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("decode Run control record {}", path.display())
    })?))
}

fn write_local_record(path: &Path, record: &RunControlRecord) -> anyhow::Result<()> {
    let parent = path.parent().context("Run control path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

async fn read_object_record(
    store: &Arc<ObjectStore>,
    path: &ObjectPath,
) -> anyhow::Result<Option<(RunControlRecord, UpdateVersion)>> {
    let result = match store.inner.get(path).await {
        Ok(result) => result,
        Err(ObjectStoreError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let version = UpdateVersion {
        e_tag: result.meta.e_tag.clone(),
        version: result.meta.version.clone(),
    };
    let bytes = result.bytes().await?;
    let record = serde_json::from_slice(&bytes)
        .with_context(|| format!("decode Run control object {path}"))?;
    Ok(Some((record, version)))
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use persisting_agentctl::RunState;

    fn commit(run: &str, attempt: &str, epoch: u64, digest: &str) -> RunCommitRequest {
        RunCommitRequest {
            run_id: RunId::new(run),
            task_id: Some("task-1".into()),
            attempt_id: AttemptId::new(attempt),
            lease_epoch: epoch,
            state: RunState::Completed,
            event_high_watermark: Some(9),
            result_digest: digest.into(),
        }
    }

    #[tokio::test]
    async fn stale_lease_cannot_win_terminal_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunControlStore::open(dir.path().to_str().unwrap())
            .await
            .unwrap();
        let run = RunId::new("run-1");
        let first = store
            .acquire_lease(&run, Some("task-1"), "worker-a", 60_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(first) = first else {
            panic!()
        };
        assert!(matches!(
            store
                .acquire_lease(&run, Some("task-1"), "worker-b", 60_000)
                .await
                .unwrap(),
            LeaseAcquireOutcome::Held(_)
        ));
        let second = store
            .takeover_lease(&run, Some("task-1"), "worker-b", 60_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(second) = second else {
            panic!()
        };
        assert_eq!(second.epoch, first.epoch + 1);
        assert!(matches!(
            store.commit_run(commit("run-1", "attempt-a", first.epoch, "sha256:a")).await.unwrap(),
            CommitRunOutcome::StaleLease { current_epoch: Some(epoch), .. } if epoch == second.epoch
        ));
        assert!(matches!(
            store
                .commit_run(commit("run-1", "attempt-b", second.epoch, "sha256:b"))
                .await
                .unwrap(),
            CommitRunOutcome::Committed(_)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_commits_have_one_visible_winner() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let store = RunControlStore::open(&root).await.unwrap();
        let run = RunId::new("run-race");
        let lease = store
            .acquire_lease(&run, Some("task-1"), "worker", 60_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(lease) = lease else {
            panic!()
        };
        let mut joins = Vec::new();
        for index in 0..8 {
            let root = root.clone();
            let epoch = lease.epoch;
            joins.push(tokio::spawn(async move {
                let store = RunControlStore::open(root).await.unwrap();
                store
                    .commit_run(commit(
                        "run-race",
                        &format!("attempt-{index}"),
                        epoch,
                        &format!("sha256:{index}"),
                    ))
                    .await
                    .unwrap()
            }));
        }
        let mut committed = 0;
        for join in joins {
            if matches!(join.await.unwrap(), CommitRunOutcome::Committed(_)) {
                committed += 1;
            }
        }
        assert_eq!(committed, 1);
        assert!(store.get(&run).await.unwrap().unwrap().commit.is_some());
    }

    #[tokio::test]
    async fn identical_commit_is_idempotent_but_different_result_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunControlStore::open(dir.path().to_str().unwrap())
            .await
            .unwrap();
        let run = RunId::new("run-idempotent");
        let lease = store
            .acquire_lease(&run, None, "worker", 10_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(lease) = lease else {
            panic!()
        };
        let request = commit("run-idempotent", "attempt-1", lease.epoch, "sha256:same");
        assert!(matches!(
            store.commit_run(request.clone()).await.unwrap(),
            CommitRunOutcome::Committed(_)
        ));
        assert!(matches!(
            store.commit_run(request).await.unwrap(),
            CommitRunOutcome::AlreadyCommitted(_)
        ));
        assert!(matches!(
            store
                .commit_run(commit(
                    "run-idempotent",
                    "attempt-2",
                    lease.epoch,
                    "sha256:other"
                ))
                .await
                .unwrap(),
            CommitRunOutcome::Conflict(_)
        ));
        assert!(matches!(
            store
                .acquire_lease(&run, None, "late-worker", 10_000)
                .await
                .unwrap(),
            LeaseAcquireOutcome::AlreadyCommitted(_)
        ));
    }

    #[tokio::test]
    async fn renewal_extends_same_epoch_and_rejects_wrong_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunControlStore::open(dir.path().to_str().unwrap())
            .await
            .unwrap();
        let run = RunId::new("run-renew");
        let lease = store
            .acquire_lease(&run, Some("task-renew"), "owner-a", 1_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(lease) = lease else {
            panic!()
        };
        assert!(!store
            .renew_lease(&run, lease.epoch, "owner-b", 2_000)
            .await
            .unwrap());
        assert!(store
            .renew_lease(&run, lease.epoch, "owner-a", 2_000)
            .await
            .unwrap());
        let renewed = store.get(&run).await.unwrap().unwrap().lease.unwrap();
        assert!(renewed.expires_at_unix_ms > lease.expires_at_unix_ms);
        assert!(matches!(
            store
                .acquire_lease(&run, Some("task-renew"), "owner-b", 2_000)
                .await
                .unwrap(),
            LeaseAcquireOutcome::Held(held) if held.epoch == lease.epoch
        ));
    }

    #[tokio::test]
    async fn renewal_cannot_revive_an_expired_lease() {
        let dir = tempfile::tempdir().unwrap();
        let store = RunControlStore::open(dir.path().to_str().unwrap())
            .await
            .unwrap();
        let run = RunId::new("run-expired-renewal");
        let lease = store
            .acquire_lease(&run, Some("task-expired"), "owner", 5)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(lease) = lease else {
            panic!()
        };
        while unix_now_ms() <= lease.expires_at_unix_ms {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        assert!(!store
            .renew_lease(&run, lease.epoch, "owner", 10_000)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn object_store_backend_uses_conditional_create_and_update() {
        let root = format!(
            "shared-memory://run-control-{}-{}/root",
            std::process::id(),
            unix_now_ms()
        );
        let store = RunControlStore::open(root).await.unwrap();
        let run = RunId::new("run-object-cas");
        let lease = store
            .acquire_lease(&run, Some("task-object"), "owner", 10_000)
            .await
            .unwrap();
        let LeaseAcquireOutcome::Acquired(lease) = lease else {
            panic!()
        };
        assert!(store
            .renew_lease(&run, lease.epoch, "owner", 10_000)
            .await
            .unwrap());
        assert!(matches!(
            store
                .commit_run(commit(
                    "run-object-cas",
                    "attempt-object",
                    lease.epoch,
                    "sha256:object"
                ))
                .await
                .unwrap(),
            CommitRunOutcome::Committed(_)
        ));
        assert!(store.get(&run).await.unwrap().unwrap().commit.is_some());
    }
}
