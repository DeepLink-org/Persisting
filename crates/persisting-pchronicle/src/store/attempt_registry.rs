//! Durable pVisor Attempt liveness and terminal-result registry.
//!
//! One CAS-managed record exists per Run. A newer lease epoch fences every
//! update from an older Attempt, while heartbeat expiry lets pPilot distinguish
//! a live remote Attempt from an orphan after coordinator restart.

use anyhow::{bail, Context};
use fs2::FileExt;
use lance::io::ObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectStoreError, ObjectStoreExt, PutMode, UpdateVersion};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ATTEMPT_DIR: &str = "attempt-registry";
const CAS_RETRIES: usize = 32;

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

enum Backend {
    Local(PathBuf),
    Object {
        store: Arc<ObjectStore>,
        root: ObjectPath,
    },
}

pub struct AttemptRegistry {
    root_uri: String,
    backend: Backend,
}

impl std::fmt::Debug for AttemptRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttemptRegistry")
            .field("root_uri", &self.root_uri)
            .finish_non_exhaustive()
    }
}

impl AttemptRegistry {
    pub async fn open(root: impl AsRef<str>) -> anyhow::Result<Self> {
        let root = root.as_ref().trim();
        if root.is_empty() {
            bail!("Attempt registry root must not be empty");
        }
        if !root.contains("://") {
            let path = PathBuf::from(root).join(ATTEMPT_DIR);
            tokio::fs::create_dir_all(&path)
                .await
                .with_context(|| format!("create Attempt registry root {}", path.display()))?;
            return Ok(Self {
                root_uri: root.to_string(),
                backend: Backend::Local(path),
            });
        }
        let (store, object_root) = ObjectStore::from_uri(root)
            .await
            .with_context(|| format!("open Attempt registry object store {root}"))?;
        Ok(Self {
            root_uri: root.to_string(),
            backend: Backend::Object {
                store,
                root: object_root.join(ATTEMPT_DIR),
            },
        })
    }

    pub fn root_uri(&self) -> &str {
        &self.root_uri
    }

    pub async fn publish_active(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> anyhow::Result<bool> {
        validate_identity(run_id, attempt_id, lease_epoch)?;
        let run_id = run_id.to_string();
        let attempt_id = attempt_id.to_string();
        self.mutate(&run_id.clone(), move |current| {
            if current.is_some_and(|record| {
                record.lease_epoch > lease_epoch
                    || (record.lease_epoch == lease_epoch
                        && (record.attempt_id != attempt_id
                            || record.state == AttemptRecordState::Terminal))
            }) {
                return Ok(Mutation::Unchanged(false));
            }
            let now = unix_now_ms();
            let record = AttemptRecord {
                revision: next_revision(current)?,
                run_id: run_id.clone(),
                attempt_id: attempt_id.clone(),
                lease_epoch,
                state: AttemptRecordState::Active,
                heartbeat_at_unix_ms: now,
                expires_at_unix_ms: now.saturating_add(ttl_ms.max(1)),
                terminal_result: None,
            };
            Ok(Mutation::Replace(record, true))
        })
        .await
    }

    pub async fn heartbeat(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        ttl_ms: u64,
    ) -> anyhow::Result<bool> {
        validate_identity(run_id, attempt_id, lease_epoch)?;
        let run_id = run_id.to_string();
        let attempt_id = attempt_id.to_string();
        self.mutate(&run_id.clone(), move |current| {
            let Some(current) = current else {
                return Ok(Mutation::Unchanged(false));
            };
            if current.run_id != run_id
                || current.attempt_id != attempt_id
                || current.lease_epoch != lease_epoch
                || current.state != AttemptRecordState::Active
            {
                return Ok(Mutation::Unchanged(false));
            }
            let now = unix_now_ms();
            let mut record = current.clone();
            record.revision = next_revision(Some(current))?;
            record.heartbeat_at_unix_ms = now;
            record.expires_at_unix_ms = now.saturating_add(ttl_ms.max(1));
            Ok(Mutation::Replace(record, true))
        })
        .await
    }

    pub async fn publish_terminal(
        &self,
        run_id: &str,
        attempt_id: &str,
        lease_epoch: u64,
        result: Value,
    ) -> anyhow::Result<bool> {
        validate_identity(run_id, attempt_id, lease_epoch)?;
        let run_id = run_id.to_string();
        let attempt_id = attempt_id.to_string();
        self.mutate(&run_id.clone(), move |current| {
            let Some(current) = current else {
                return Ok(Mutation::Unchanged(false));
            };
            if current.run_id != run_id
                || current.attempt_id != attempt_id
                || current.lease_epoch != lease_epoch
            {
                return Ok(Mutation::Unchanged(false));
            }
            if current.state == AttemptRecordState::Terminal {
                return Ok(Mutation::Unchanged(
                    current.terminal_result.as_ref() == Some(&result),
                ));
            }
            let now = unix_now_ms();
            let mut record = current.clone();
            record.revision = next_revision(Some(current))?;
            record.state = AttemptRecordState::Terminal;
            record.heartbeat_at_unix_ms = now;
            record.expires_at_unix_ms = now;
            record.terminal_result = Some(result.clone());
            Ok(Mutation::Replace(record, true))
        })
        .await
    }

    pub async fn get(&self, run_id: &str) -> anyhow::Result<Option<AttemptRecord>> {
        match &self.backend {
            Backend::Local(root) => read_local_record(&record_path(root, run_id)),
            Backend::Object { store, root } => {
                Ok(read_object_record(store, &object_record_path(root, run_id))
                    .await?
                    .map(|(record, _)| record))
            }
        }
    }

    async fn mutate<T, F>(&self, run_id: &str, mutate: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: Fn(Option<&AttemptRecord>) -> anyhow::Result<Mutation<T>> + Send + Sync + 'static,
    {
        match &self.backend {
            Backend::Local(root) => {
                let path = record_path(root, run_id);
                let lock_path = path.with_extension("lock");
                tokio::task::spawn_blocking(move || {
                    let lock = OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&lock_path)?;
                    lock.lock_exclusive()?;
                    let current = read_local_record(&path)?;
                    let outcome = mutate(current.as_ref())?;
                    let value = match outcome {
                        Mutation::Unchanged(value) => value,
                        Mutation::Replace(record, value) => {
                            write_local_record(&path, &record)?;
                            value
                        }
                    };
                    FileExt::unlock(&lock)?;
                    Ok(value)
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
                bail!("Attempt registry CAS contention exceeded {CAS_RETRIES} retries")
            }
        }
    }
}

enum Mutation<T> {
    Unchanged(T),
    Replace(AttemptRecord, T),
}

fn validate_identity(run_id: &str, attempt_id: &str, lease_epoch: u64) -> anyhow::Result<()> {
    if run_id.trim().is_empty() || attempt_id.trim().is_empty() || lease_epoch == 0 {
        bail!("Attempt registry requires run_id, attempt_id, and non-zero lease_epoch");
    }
    Ok(())
}

fn next_revision(current: Option<&AttemptRecord>) -> anyhow::Result<u64> {
    current
        .map(|record| record.revision)
        .unwrap_or(0)
        .checked_add(1)
        .context("Attempt record revision overflow")
}

fn encoded_id(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(char::from(byte));
            }
            other => encoded.push_str(&format!("~{other:02x}")),
        }
    }
    encoded
}

fn record_path(root: &Path, run_id: &str) -> PathBuf {
    root.join(format!("{}.json", encoded_id(run_id)))
}

fn object_record_path(root: &ObjectPath, run_id: &str) -> ObjectPath {
    root.clone().join(format!("{}.json", encoded_id(run_id)))
}

fn read_local_record(path: &Path) -> anyhow::Result<Option<AttemptRecord>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("decode Attempt record {}", path.display())
    })?))
}

fn write_local_record(path: &Path, record: &AttemptRecord) -> anyhow::Result<()> {
    let parent = path.parent().context("Attempt record path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attempt"),
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
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
) -> anyhow::Result<Option<(AttemptRecord, UpdateVersion)>> {
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
        .with_context(|| format!("decode Attempt registry object {path}"))?;
    Ok(Some((record, version)))
}

pub fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn newer_epoch_fences_old_attempt_heartbeats_and_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AttemptRegistry::open(dir.path().to_str().unwrap())
            .await
            .unwrap();
        assert!(registry
            .publish_active("run-1", "attempt-1", 1, 10_000)
            .await
            .unwrap());
        assert!(registry
            .publish_active("run-1", "attempt-2", 2, 10_000)
            .await
            .unwrap());
        assert!(!registry
            .heartbeat("run-1", "attempt-1", 1, 10_000)
            .await
            .unwrap());
        assert!(!registry
            .publish_terminal("run-1", "attempt-1", 1, serde_json::json!({"old": true}))
            .await
            .unwrap());
        assert!(registry
            .publish_terminal("run-1", "attempt-2", 2, serde_json::json!({"ok": true}))
            .await
            .unwrap());
        let record = registry.get("run-1").await.unwrap().unwrap();
        assert_eq!(record.state, AttemptRecordState::Terminal);
        assert_eq!(record.attempt_id, "attempt-2");
    }
}
