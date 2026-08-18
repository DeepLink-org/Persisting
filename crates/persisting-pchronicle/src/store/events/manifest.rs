//! Epoch-fenced visibility manifest for the canonical event log.
//!
//! Writers append to a private Lance dataset and publish its immutable version
//! through this manifest. A newer epoch changes the active fence before it
//! writes data, so an older writer can create garbage but cannot make it
//! visible to readers.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use fs2::FileExt;
use futures::TryStreamExt;
use lance::io::ObjectStore;
use object_store::path::Path as ObjectPath;
use object_store::{Error as ObjectStoreError, ObjectStoreExt, PutMode, UpdateVersion};
use serde::{Deserialize, Serialize};

const MANIFEST_FILE: &str = "_manifest.json";
const MANIFEST_LOCK_FILE: &str = "_manifest.lock";
const CAS_RETRIES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventWriterFence {
    pub epoch: u64,
    pub writer_id: String,
}

impl EventWriterFence {
    pub fn new(epoch: u64, writer_id: impl Into<String>) -> Result<Self> {
        let writer_id = writer_id.into();
        anyhow::ensure!(epoch > 0, "event writer fence epoch must be non-zero");
        anyhow::ensure!(
            !writer_id.trim().is_empty(),
            "event writer fence writer_id must not be empty"
        );
        Ok(Self { epoch, writer_id })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventWriterConflict {
    StaleFence,
    EpochAlreadyOwned,
    PublicationChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ManifestWriteOutcome<T> {
    Applied(T),
    Conflict(EventWriterConflict),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EventSegment {
    pub id: String,
    pub version: u64,
    pub rows: u64,
    pub level: u8,
    pub sealed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EventManifest {
    /// Physical manifest revision. Writer fencing, layout maintenance, and
    /// fact publication may all advance this value.
    pub revision: u64,
    /// Logical fact revision. Only publishing newly visible rows advances it.
    pub fact_version: u64,
    /// Number of canonical rows visible at `fact_version`.
    pub fact_rows: u64,
    pub active_writer: EventWriterFence,
    #[serde(default)]
    pub segments: Vec<EventSegment>,
}

impl EventManifest {
    pub fn total_rows(&self) -> u64 {
        self.segments.iter().map(|segment| segment.rows).sum()
    }
}

pub(super) async fn read(root_uri: &str) -> Result<Option<EventManifest>> {
    if is_object_store_uri(root_uri) {
        let (store, root) = object_backend(root_uri).await?;
        Ok(read_object_manifest(&store, &root.join(MANIFEST_FILE))
            .await?
            .map(|(manifest, _)| manifest))
    } else {
        read_local_manifest(&Path::new(root_uri).join(MANIFEST_FILE))
    }
}

pub(super) async fn activate(
    root_uri: &str,
    requested: Option<&EventWriterFence>,
    auto_writer_id: &str,
) -> Result<ManifestWriteOutcome<EventManifest>> {
    let requested = requested.cloned();
    let auto_writer_id = auto_writer_id.to_string();
    mutate(root_uri, move |current| {
        let next_fence = match (&requested, current) {
            (Some(fence), Some(manifest)) if fence == &manifest.active_writer => {
                return Ok(ManifestMutation::Unchanged(manifest.clone()));
            }
            (Some(fence), Some(manifest)) if fence.epoch < manifest.active_writer.epoch => {
                return Ok(ManifestMutation::Conflict(EventWriterConflict::StaleFence));
            }
            (Some(fence), Some(manifest)) if fence.epoch == manifest.active_writer.epoch => {
                return Ok(ManifestMutation::Conflict(
                    EventWriterConflict::EpochAlreadyOwned,
                ));
            }
            (Some(fence), _) => fence.clone(),
            (None, Some(manifest)) => EventWriterFence {
                epoch: manifest
                    .active_writer
                    .epoch
                    .checked_add(1)
                    .context("event writer epoch overflow")?,
                writer_id: auto_writer_id.clone(),
            },
            (None, None) => EventWriterFence {
                epoch: 1,
                writer_id: auto_writer_id.clone(),
            },
        };
        let mut next = current.cloned().unwrap_or(EventManifest {
            revision: 0,
            fact_version: 0,
            fact_rows: 0,
            active_writer: next_fence.clone(),
            segments: Vec::new(),
        });
        next.revision = next
            .revision
            .checked_add(1)
            .context("event manifest revision overflow")?;
        next.active_writer = next_fence;
        Ok(ManifestMutation::Replace(next.clone(), next))
    })
    .await
}

pub(super) async fn publish_segment(
    root_uri: &str,
    fence: &EventWriterFence,
    segment: EventSegment,
) -> Result<ManifestWriteOutcome<EventManifest>> {
    let fence = fence.clone();
    mutate(root_uri, move |current| {
        let current = current.context("event manifest disappeared during publish")?;
        if let Some(conflict) = active_writer_conflict(current, &fence) {
            return Ok(ManifestMutation::Conflict(conflict));
        }
        if let Some(existing) = current
            .segments
            .iter()
            .find(|existing| existing.id == segment.id)
        {
            if existing == &segment {
                return Ok(ManifestMutation::Unchanged(current.clone()));
            }
            anyhow::ensure!(
                segment.version >= existing.version && segment.rows >= existing.rows,
                "event segment publication cannot move version or row count backwards"
            );
            anyhow::ensure!(
                segment.level == existing.level,
                "event segment publication cannot change compaction level"
            );
            anyhow::ensure!(
                !existing.sealed || segment.sealed,
                "event segment publication cannot reopen a sealed segment"
            );
            anyhow::ensure!(
                segment.version > existing.version
                    || (segment.version == existing.version
                        && segment.rows == existing.rows
                        && !existing.sealed
                        && segment.sealed),
                "event segment publication must advance version or seal the segment"
            );
        }
        let mut next = current.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .context("event manifest revision overflow")?;
        match next
            .segments
            .iter_mut()
            .find(|existing| existing.id == segment.id)
        {
            Some(existing) => *existing = segment.clone(),
            None => next.segments.push(segment.clone()),
        }
        let visible_rows = next.total_rows();
        if visible_rows > current.fact_rows {
            next.fact_version = current
                .fact_version
                .checked_add(1)
                .context("event fact version overflow")?;
            next.fact_rows = visible_rows;
        }
        Ok(ManifestMutation::Replace(next.clone(), next))
    })
    .await
}

pub(super) async fn replace_segments(
    root_uri: &str,
    fence: &EventWriterFence,
    segments: Vec<EventSegment>,
) -> Result<ManifestWriteOutcome<EventManifest>> {
    let fence = fence.clone();
    mutate(root_uri, move |current| {
        let current = current.context("event manifest disappeared during maintenance")?;
        if let Some(conflict) = active_writer_conflict(current, &fence) {
            return Ok(ManifestMutation::Conflict(conflict));
        }
        let replacement_rows = segments.iter().try_fold(0_u64, |total, segment| {
            total
                .checked_add(segment.rows)
                .context("event segment replacement row count overflow")
        })?;
        anyhow::ensure!(
            replacement_rows == current.fact_rows,
            "event maintenance must preserve visible fact rows"
        );
        let mut next = current.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .context("event manifest revision overflow")?;
        next.segments.clone_from(&segments);
        Ok(ManifestMutation::Replace(next.clone(), next))
    })
    .await
}

/// Atomically replace one contiguous immutable segment group while preserving
/// its position in append order. Exact descriptor matching prevents a stale
/// compactor from overwriting a segment version published by another task.
pub(super) async fn replace_segment_group(
    root_uri: &str,
    fence: &EventWriterFence,
    expected: &[EventSegment],
    replacement: EventSegment,
) -> Result<ManifestWriteOutcome<EventManifest>> {
    anyhow::ensure!(!expected.is_empty(), "segment replacement group is empty");
    let expected = expected.to_vec();
    let fence = fence.clone();
    mutate(root_uri, move |current| {
        let current = current.context("event manifest disappeared during segment merge")?;
        if let Some(conflict) = active_writer_conflict(current, &fence) {
            return Ok(ManifestMutation::Conflict(conflict));
        }
        let Some(start) = current
            .segments
            .windows(expected.len())
            .position(|window| window == expected.as_slice())
        else {
            return Ok(ManifestMutation::Conflict(
                EventWriterConflict::PublicationChanged,
            ));
        };
        let expected_rows = expected.iter().try_fold(0_u64, |total, segment| {
            total
                .checked_add(segment.rows)
                .context("event segment merge row count overflow")
        })?;
        anyhow::ensure!(
            replacement.rows == expected_rows,
            "event segment merge must preserve row count"
        );

        let mut next = current.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .context("event manifest revision overflow")?;
        next.segments.splice(
            start..start + expected.len(),
            std::iter::once(replacement.clone()),
        );
        Ok(ManifestMutation::Replace(next.clone(), next))
    })
    .await
}

pub(super) fn segment_uri(root_uri: &str, segment_id: &str) -> String {
    format!(
        "{}/segments/{segment_id}.lance",
        root_uri.trim_end_matches('/')
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SegmentCleanupStats {
    pub segments_removed: u64,
    pub bytes_removed: u64,
}

pub(super) async fn cleanup_unreferenced_segments(
    root_uri: &str,
    visible_segments: &[EventSegment],
    retention: Duration,
) -> Result<SegmentCleanupStats> {
    let visible = visible_segments
        .iter()
        .map(|segment| format!("{}.lance", segment.id))
        .collect::<std::collections::BTreeSet<_>>();
    if !is_object_store_uri(root_uri) {
        let root = PathBuf::from(root_uri).join("segments");
        return tokio::task::spawn_blocking(move || {
            cleanup_local_segments(&root, &visible, retention)
        })
        .await?;
    }

    let (store, root) = object_backend(root_uri).await?;
    let segments_root = root.join("segments");
    let prefix = format!("{}/", segments_root.as_ref().trim_end_matches('/'));
    let cutoff = chrono::Utc::now()
        - chrono::Duration::from_std(retention).context("event segment retention is too large")?;
    let objects = store
        .inner
        .list(Some(&segments_root))
        .try_collect::<Vec<_>>()
        .await?;
    let mut candidates: std::collections::BTreeMap<String, (bool, u64)> =
        std::collections::BTreeMap::new();
    for object in objects {
        let Some(relative) = object.location.as_ref().strip_prefix(&prefix) else {
            continue;
        };
        let Some(directory) = relative.split('/').next() else {
            continue;
        };
        if directory.is_empty() || visible.contains(directory) {
            continue;
        }
        let entry = candidates.entry(directory.to_string()).or_insert((true, 0));
        entry.0 &= object.last_modified <= cutoff;
        entry.1 = entry.1.saturating_add(object.size);
    }
    let mut stats = SegmentCleanupStats::default();
    for (directory, (expired, bytes)) in candidates {
        if !expired {
            continue;
        }
        store
            .remove_dir_all(segments_root.clone().join(directory))
            .await
            .context("remove unreferenced event segment")?;
        stats.segments_removed += 1;
        stats.bytes_removed = stats.bytes_removed.saturating_add(bytes);
    }
    Ok(stats)
}

fn cleanup_local_segments(
    root: &Path,
    visible: &std::collections::BTreeSet<String>,
    retention: Duration,
) -> Result<SegmentCleanupStats> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SegmentCleanupStats::default());
        }
        Err(error) => return Err(error.into()),
    };
    let cutoff = SystemTime::now()
        .checked_sub(retention)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut stats = SegmentCleanupStats::default();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if visible.contains(&name) || !name.ends_with(".lance") {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH) > cutoff {
            continue;
        }
        let bytes = directory_bytes(&entry.path())?;
        std::fs::remove_dir_all(entry.path())?;
        stats.segments_removed += 1;
        stats.bytes_removed = stats.bytes_removed.saturating_add(bytes);
    }
    Ok(stats)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            bytes = bytes.saturating_add(directory_bytes(&entry.path())?);
        } else {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(bytes)
}

fn active_writer_conflict(
    manifest: &EventManifest,
    fence: &EventWriterFence,
) -> Option<EventWriterConflict> {
    (&manifest.active_writer != fence).then_some(EventWriterConflict::StaleFence)
}

enum ManifestMutation<T> {
    Unchanged(T),
    Replace(EventManifest, T),
    Conflict(EventWriterConflict),
}

async fn mutate<T, F>(root_uri: &str, mutation: F) -> Result<ManifestWriteOutcome<T>>
where
    T: Send + 'static,
    F: Fn(Option<&EventManifest>) -> Result<ManifestMutation<T>> + Send + Sync + 'static,
{
    if !is_object_store_uri(root_uri) {
        let root = PathBuf::from(root_uri);
        return tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            let lock_path = root.join(MANIFEST_LOCK_FILE);
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&lock_path)
                .with_context(|| format!("open event manifest lock {}", lock_path.display()))?;
            lock.lock_exclusive()
                .with_context(|| format!("lock event manifest {}", lock_path.display()))?;
            let manifest_path = root.join(MANIFEST_FILE);
            let current = read_local_manifest(&manifest_path)?;
            let result = match mutation(current.as_ref())? {
                ManifestMutation::Unchanged(value) => ManifestWriteOutcome::Applied(value),
                ManifestMutation::Replace(manifest, value) => {
                    validate_manifest(&manifest)?;
                    write_local_manifest(&manifest_path, &manifest)?;
                    ManifestWriteOutcome::Applied(value)
                }
                ManifestMutation::Conflict(conflict) => ManifestWriteOutcome::Conflict(conflict),
            };
            FileExt::unlock(&lock)?;
            Ok(result)
        })
        .await?;
    }

    let (store, root) = object_backend(root_uri).await?;
    let path = root.join(MANIFEST_FILE);
    for _ in 0..CAS_RETRIES {
        let current = read_object_manifest(&store, &path).await?;
        let outcome = mutation(current.as_ref().map(|(manifest, _)| manifest))?;
        let (manifest, value) = match outcome {
            ManifestMutation::Unchanged(value) => {
                return Ok(ManifestWriteOutcome::Applied(value));
            }
            ManifestMutation::Replace(manifest, value) => (manifest, value),
            ManifestMutation::Conflict(conflict) => {
                return Ok(ManifestWriteOutcome::Conflict(conflict));
            }
        };
        validate_manifest(&manifest)?;
        let mode = match current {
            None => PutMode::Create,
            Some((_, version)) => PutMode::Update(version),
        };
        let bytes = serde_json::to_vec_pretty(&manifest)?;
        match store.inner.put_opts(&path, bytes.into(), mode.into()).await {
            Ok(_) => return Ok(ManifestWriteOutcome::Applied(value)),
            Err(ObjectStoreError::AlreadyExists { .. })
            | Err(ObjectStoreError::Precondition { .. }) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(ManifestWriteOutcome::Conflict(
        EventWriterConflict::PublicationChanged,
    ))
}

fn validate_manifest(manifest: &EventManifest) -> Result<()> {
    EventWriterFence::new(
        manifest.active_writer.epoch,
        manifest.active_writer.writer_id.clone(),
    )?;
    anyhow::ensure!(
        manifest.fact_rows == manifest.total_rows(),
        "event manifest fact_rows {} does not match visible segment rows {}",
        manifest.fact_rows,
        manifest.total_rows()
    );
    let mut ids = std::collections::BTreeSet::new();
    for segment in &manifest.segments {
        anyhow::ensure!(!segment.id.is_empty(), "event segment id must not be empty");
        anyhow::ensure!(
            segment.version > 0,
            "event segment version must be non-zero"
        );
        anyhow::ensure!(ids.insert(&segment.id), "duplicate event segment id");
    }
    Ok(())
}

fn decode_manifest(bytes: &[u8], context: impl std::fmt::Display) -> Result<EventManifest> {
    let manifest: EventManifest = serde_json::from_slice(bytes)
        .with_context(|| format!("decode event manifest {context}"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn read_local_manifest(path: &Path) -> Result<Option<EventManifest>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let manifest = decode_manifest(&bytes, path.display())?;
    Ok(Some(manifest))
}

fn write_local_manifest(path: &Path, manifest: &EventManifest) -> Result<()> {
    let parent = path.parent().context("event manifest path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{MANIFEST_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(manifest)?)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

async fn object_backend(root_uri: &str) -> Result<(Arc<ObjectStore>, ObjectPath)> {
    let (store, root) = ObjectStore::from_uri(root_uri)
        .await
        .with_context(|| format!("open event manifest object store {root_uri}"))?;
    Ok((store, root))
}

async fn read_object_manifest(
    store: &Arc<ObjectStore>,
    path: &ObjectPath,
) -> Result<Option<(EventManifest, UpdateVersion)>> {
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
    let manifest = decode_manifest(&bytes, format_args!("object {path}"))?;
    Ok(Some((manifest, version)))
}

fn is_object_store_uri(uri: &str) -> bool {
    uri.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn applied<T>(outcome: ManifestWriteOutcome<T>) -> T {
        let ManifestWriteOutcome::Applied(value) = outcome else {
            panic!("manifest write unexpectedly conflicted")
        };
        value
    }

    #[tokio::test]
    async fn stale_fence_is_a_conflict_while_object_store_failure_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("events.lance");
        let uri = root.to_str().unwrap();
        let old = EventWriterFence::new(1, "old").unwrap();
        let new = EventWriterFence::new(2, "new").unwrap();
        activate(uri, Some(&old), "unused").await.unwrap();
        publish_segment(
            uri,
            &old,
            EventSegment {
                id: "old-segment".into(),
                version: 1,
                rows: 10,
                level: 0,
                sealed: false,
            },
        )
        .await
        .unwrap();
        activate(uri, Some(&new), "unused").await.unwrap();
        let outcome = publish_segment(
            uri,
            &old,
            EventSegment {
                id: "old-segment".into(),
                version: 2,
                rows: 20,
                level: 0,
                sealed: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            outcome,
            ManifestWriteOutcome::Conflict(EventWriterConflict::StaleFence)
        );
        let manifest = read(uri).await.unwrap().unwrap();
        assert_eq!(manifest.active_writer, new);
        assert_eq!(manifest.total_rows(), 10);

        assert!(
            activate("unsupported-object-store://manifest", None, "writer")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fact_version_changes_only_when_visible_rows_advance() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("events.lance");
        let uri = root.to_str().unwrap();
        let first = EventWriterFence::new(1, "first").unwrap();
        let activated = applied(activate(uri, Some(&first), "unused").await.unwrap());
        assert_eq!((activated.fact_version, activated.fact_rows), (0, 0));

        let published = applied(
            publish_segment(
                uri,
                &first,
                EventSegment {
                    id: "segment".into(),
                    version: 1,
                    rows: 3,
                    level: 0,
                    sealed: false,
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!((published.fact_version, published.fact_rows), (1, 3));

        let second = EventWriterFence::new(2, "second").unwrap();
        let reactivated = applied(activate(uri, Some(&second), "unused").await.unwrap());
        assert!(reactivated.revision > published.revision);
        assert_eq!(reactivated.fact_version, published.fact_version);
        assert_eq!(reactivated.fact_rows, published.fact_rows);

        let maintained = applied(
            replace_segments(uri, &second, reactivated.segments.clone())
                .await
                .unwrap(),
        );
        assert!(maintained.revision > reactivated.revision);
        assert_eq!(maintained.fact_version, published.fact_version);
        assert_eq!(maintained.fact_rows, published.fact_rows);
    }

    #[tokio::test]
    async fn same_epoch_cannot_be_claimed_by_another_writer() {
        let uri = format!(
            "shared-memory://event-manifest-fence-{}/events.lance",
            uuid::Uuid::new_v4()
        );
        let first = EventWriterFence::new(7, "first").unwrap();
        let second = EventWriterFence::new(7, "second").unwrap();
        activate(&uri, Some(&first), "unused").await.unwrap();
        assert_eq!(
            activate(&uri, Some(&second), "unused").await.unwrap(),
            ManifestWriteOutcome::Conflict(EventWriterConflict::EpochAlreadyOwned)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn object_store_cas_serializes_concurrent_epoch_claims() {
        const WRITERS: usize = 32;
        let uri = format!(
            "shared-memory://event-manifest-contention-{}/events.lance",
            uuid::Uuid::new_v4()
        );
        let mut claims = Vec::with_capacity(WRITERS);
        for writer in 0..WRITERS {
            let uri = uri.clone();
            claims.push(tokio::spawn(async move {
                activate(&uri, None, &format!("writer-{writer}"))
                    .await
                    .map(|outcome| applied(outcome).active_writer.epoch)
            }));
        }
        let mut epochs = Vec::with_capacity(WRITERS);
        for claim in claims {
            epochs.push(claim.await.unwrap().unwrap());
        }
        epochs.sort_unstable();
        assert_eq!(epochs, (1..=WRITERS as u64).collect::<Vec<_>>());
        let manifest = read(&uri).await.unwrap().unwrap();
        assert_eq!(manifest.active_writer.epoch, WRITERS as u64);
        assert_eq!(manifest.revision, WRITERS as u64);
    }

    #[tokio::test]
    async fn manifest_metadata_is_constant_in_micro_batch_count_and_publish_is_idempotent() {
        const PUBLISHES: u64 = 256;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("events.lance");
        let uri = root.to_str().unwrap();
        let fence = EventWriterFence::new(1, "writer").unwrap();
        activate(uri, Some(&fence), "unused").await.unwrap();
        publish_segment(
            uri,
            &fence,
            EventSegment {
                id: "one-writer-segment".into(),
                version: 1,
                rows: 1,
                level: 0,
                sealed: false,
            },
        )
        .await
        .unwrap();
        let initial_bytes = std::fs::metadata(root.join(MANIFEST_FILE)).unwrap().len();

        for version in 2..=PUBLISHES {
            publish_segment(
                uri,
                &fence,
                EventSegment {
                    id: "one-writer-segment".into(),
                    version,
                    rows: version,
                    level: 0,
                    sealed: false,
                },
            )
            .await
            .unwrap();
        }
        let before_retry = read(uri).await.unwrap().unwrap();
        let idempotent = applied(
            publish_segment(
                uri,
                &fence,
                EventSegment {
                    id: "one-writer-segment".into(),
                    version: PUBLISHES,
                    rows: PUBLISHES,
                    level: 0,
                    sealed: false,
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(idempotent.revision, before_retry.revision);
        assert_eq!(idempotent.segments.len(), 1);
        assert_eq!(idempotent.total_rows(), PUBLISHES);

        let final_bytes = std::fs::metadata(root.join(MANIFEST_FILE)).unwrap().len();
        assert!(
            final_bytes <= initial_bytes + 32,
            "manifest bytes must grow only with integer digit width: initial={initial_bytes}, final={final_bytes}"
        );
    }

    #[tokio::test]
    async fn contiguous_segment_group_replacement_preserves_order_and_rows() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("events.lance");
        let uri = root.to_str().unwrap();
        let fence = EventWriterFence::new(1, "writer").unwrap();
        activate(uri, Some(&fence), "unused").await.unwrap();
        for id in ["a", "b", "c"] {
            publish_segment(
                uri,
                &fence,
                EventSegment {
                    id: id.into(),
                    version: 1,
                    rows: 2,
                    level: 0,
                    sealed: true,
                },
            )
            .await
            .unwrap();
        }
        let before = read(uri).await.unwrap().unwrap();
        let merged = applied(
            replace_segment_group(
                uri,
                &fence,
                &before.segments[..2],
                EventSegment {
                    id: "ab".into(),
                    version: 1,
                    rows: 4,
                    level: 1,
                    sealed: true,
                },
            )
            .await
            .unwrap(),
        );
        assert_eq!(
            merged
                .segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            ["ab", "c"]
        );
        assert_eq!(merged.total_rows(), before.total_rows());

        let stale_outcome = replace_segment_group(
            uri,
            &fence,
            &before.segments[..2],
            EventSegment {
                id: "stale".into(),
                version: 1,
                rows: 4,
                level: 1,
                sealed: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            stale_outcome,
            ManifestWriteOutcome::Conflict(EventWriterConflict::PublicationChanged)
        );
    }

    #[tokio::test]
    async fn malformed_manifest_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("events.lance");
        let uri = root.to_str().unwrap();
        activate(uri, None, "writer").await.unwrap();
        std::fs::write(root.join(MANIFEST_FILE), b"{truncated").unwrap();
        let error = read(uri).await.unwrap_err();
        assert!(error.to_string().contains("decode event manifest"));
    }

    #[test]
    fn manifest_wire_has_no_schema_marker() {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "revision": 7,
            "fact_version": 7,
            "fact_rows": 0,
            "active_writer": {"epoch": 1, "writer_id": "writer"},
            "segments": []
        }))
        .unwrap();
        let manifest = decode_manifest(&bytes, "manifest fixture").unwrap();
        let value = serde_json::to_value(manifest).unwrap();
        assert!(value.get("schema_version").is_none());
    }

    #[tokio::test]
    async fn object_store_cleanup_removes_only_unreferenced_segment_prefixes() {
        let uri = format!(
            "shared-memory://event-segment-cleanup-{}/events.lance",
            uuid::Uuid::new_v4()
        );
        let (store, root) = object_backend(&uri).await.unwrap();
        let orphan = root
            .clone()
            .join("segments")
            .join("orphan.lance")
            .join("data")
            .join("file.lance");
        let visible = root
            .clone()
            .join("segments")
            .join("visible.lance")
            .join("data")
            .join("file.lance");
        store.inner.put(&orphan, "orphan".into()).await.unwrap();
        store.inner.put(&visible, "visible".into()).await.unwrap();

        let stats = cleanup_unreferenced_segments(
            &uri,
            &[EventSegment {
                id: "visible".into(),
                version: 1,
                rows: 1,
                level: 0,
                sealed: false,
            }],
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(stats.segments_removed, 1);
        assert!(matches!(
            store.inner.get(&orphan).await,
            Err(ObjectStoreError::NotFound { .. })
        ));
        assert!(store.inner.get(&visible).await.is_ok());
    }
}
