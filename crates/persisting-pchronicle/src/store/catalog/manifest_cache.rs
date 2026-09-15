//! Rebuildable cache of manifest-derived directory observations.
//!
//! This module deliberately knows nothing about DataFusion.  It is the single
//! boundary used by navigational callers that can tolerate a stale snapshot.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

const MAX_REFRESH_DIRECTORIES: usize = 10_000;
const REFRESH_MOUNT_DEADLINE: Duration = Duration::from_secs(120);
const REFRESH_DIRECTORY_TIMEOUT: Duration = Duration::from_secs(20);

use crate::store::{DatasetLocation, PathListEntry, PersistentCache};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestListing {
    #[serde(default)]
    pub partial: bool,
    pub entries: Vec<PathListEntry>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSummary {
    #[serde(default)]
    pub partial: bool,
    pub datasets: u64,
    pub trajectories: u64,
}

/// Outcome of one bounded mount walk. A partial walk is usable, but not complete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestRefreshReport {
    pub refreshed_directories: usize,
    pub partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestReadMode {
    Cached,
    RefreshIfMissing,
    Fresh,
}

/// Persistent manifest cache. Keys are caller-owned stable identities, e.g.
/// `mount-uri\0relative-prefix`; this keeps the cache independent of UI types.
#[derive(Clone)]
pub struct ManifestCache {
    disk: Arc<PersistentCache<String, serde_json::Value>>,
    values: Arc<RwLock<std::collections::HashMap<String, ManifestListing>>>,
}

impl ManifestCache {
    pub async fn open(path: PathBuf) -> Self {
        let disk = Arc::new(PersistentCache::open(path).await);
        let values = Arc::new(RwLock::new(
            disk.values()
                .await
                .into_iter()
                .filter_map(|(key, value)| serde_json::from_value(value).ok().map(|v| (key, v)))
                .collect(),
        ));
        Self { disk, values }
    }

    pub async fn get(&self, key: &str) -> Option<ManifestListing> {
        self.values.read().await.get(key).cloned()
    }

    pub async fn get_or_refresh(
        &self,
        key: impl Into<String> + Clone,
        location: &DatasetLocation,
        prefix: &str,
        mode: ManifestReadMode,
    ) -> Result<Option<ManifestListing>> {
        let key = key.into();
        if !matches!(mode, ManifestReadMode::Fresh) {
            if let Some(value) = self.get(&key).await {
                return Ok(Some(value));
            }
            if matches!(mode, ManifestReadMode::Cached) {
                return Ok(None);
            }
        }
        self.refresh(key, location, prefix).await.map(Some)
    }

    /// Read one level from the authoritative location and publish it
    /// atomically. Callers may use this for a foreground miss or a worker.
    pub async fn refresh(
        &self,
        key: impl Into<String>,
        location: &DatasetLocation,
        prefix: &str,
    ) -> Result<ManifestListing> {
        self.refresh_impl(key.into(), location, prefix, false).await
    }

    /// Observe remote child names now; the browse worker resolves each child's
    /// type and statistics when visiting that prefix, without delaying its parent.
    pub async fn refresh_for_browse(
        &self,
        key: impl Into<String>,
        location: &DatasetLocation,
        prefix: &str,
    ) -> Result<ManifestListing> {
        self.refresh_impl(key.into(), location, prefix, true).await
    }

    async fn refresh_impl(
        &self,
        key: String,
        location: &DatasetLocation,
        prefix: &str,
        browse: bool,
    ) -> Result<ManifestListing> {
        let refresh_gate =
            crate::store::root_write_lock::for_root(&serde_json::to_string(&(self.path(), &key))?);
        let _guard = refresh_gate.lock().await;
        let listing = ManifestListing {
            partial: false,
            entries: if browse {
                location.list_for_browse(prefix).await?
            } else {
                location.list(prefix).await?
            },
            observed_at: chrono::Utc::now().timestamp(),
        };
        self.values
            .write()
            .await
            .insert(key.clone(), listing.clone());
        if let Err(error) = self
            .disk
            .upsert(&key, &serde_json::to_value(&listing)?, &[])
            .await
        {
            tracing::warn!(target: "pchronicle.serve", prefix, %error,
                "manifest cache persistence failed; using memory");
        }
        // `key` contains NUL separators and is an internal cache identity;
        // logging it makes journald truncate the record at the dataset name.
        tracing::debug!(target: "pchronicle.serve", prefix, entries = listing.entries.len(), browse, "manifest cache updated");
        Ok(listing)
    }

    pub fn writable(&self) -> bool {
        self.disk.writable()
    }

    pub fn path(&self) -> &std::path::Path {
        self.disk.path()
    }

    pub async fn projection_values(&self) -> std::collections::HashMap<String, serde_json::Value> {
        self.disk
            .values()
            .await
            .into_iter()
            .filter(|(key, _)| serde_json::from_str::<serde_json::Value>(key).is_ok())
            .collect()
    }

    pub async fn put_projection(
        &self,
        key: impl Into<String>,
        value: &serde_json::Value,
    ) -> Result<()> {
        self.disk.upsert(&key.into(), value, &[]).await
    }

    pub async fn remove_projections(&self, keys: &[String]) -> Result<()> {
        self.disk
            .upsert(
                &"__projection_tombstone__".to_owned(),
                &serde_json::Value::Null,
                keys,
            )
            .await
    }

    /// Aggregate the currently cached manifest observations for one mount.
    /// This never performs I/O and is therefore safe for UI rendering.
    pub async fn summary(&self, key_prefix: &str) -> LocationSummary {
        self.summary_under(key_prefix).await
    }

    /// Aggregate every cached manifest at `key_prefix` and below it.
    pub async fn summary_under(&self, key_prefix: &str) -> LocationSummary {
        let values = self.values.read().await;
        let mut seen = HashSet::new();
        values
            .iter()
            .filter(|(key, _)| {
                // NUL separates the mount identity from the relative path;
                // slashes separate directories inside that path.
                key.strip_prefix(key_prefix).is_some_and(|suffix| {
                    suffix.is_empty() || suffix.starts_with('\0') || suffix.starts_with('/')
                })
            })
            .fold(LocationSummary::default(), |mut total, (_, listing)| {
                total.partial |= listing.partial;
                for entry in &listing.entries {
                    let is_dataset = matches!(entry.kind, crate::store::PathListKind::Dataset)
                        || (matches!(entry.kind, crate::store::PathListKind::File)
                            && entry.format.as_deref().is_none_or(|format| {
                                matches!(format, "storyline-lance" | "compact-jsonl/v1")
                            }));
                    if is_dataset && seen.insert(entry.path.clone()) {
                        total.datasets += 1;
                        total.trajectories += entry.record_count.unwrap_or_default();
                    }
                }
                total
            })
    }

    /// Breadth-first refresh of a mount. Refreshes serialize per observation
    /// key, allowing unrelated foreground directories to load concurrently.
    pub async fn refresh_mount(
        &self,
        key_prefix: &str,
        location: &DatasetLocation,
    ) -> Result<ManifestRefreshReport> {
        self.refresh_mount_bounded(
            key_prefix,
            location,
            MAX_REFRESH_DIRECTORIES,
            REFRESH_MOUNT_DEADLINE,
        )
        .await
    }

    async fn refresh_mount_bounded(
        &self,
        key_prefix: &str,
        location: &DatasetLocation,
        max_directories: usize,
        budget: Duration,
    ) -> Result<ManifestRefreshReport> {
        let mut queue = VecDeque::from([String::new()]);
        let mut seen = HashSet::from([String::new()]);
        let mut report = ManifestRefreshReport::default();
        let deadline = tokio::time::Instant::now() + budget;
        while let Some(prefix) = queue.pop_front() {
            if report.refreshed_directories >= max_directories
                || tokio::time::Instant::now() >= deadline
            {
                report.partial = true;
                break;
            }
            let key = if prefix.is_empty() {
                key_prefix.to_owned()
            } else {
                format!("{key_prefix}\0{prefix}")
            };
            let directory_deadline =
                deadline.min(tokio::time::Instant::now() + REFRESH_DIRECTORY_TIMEOUT);
            let listing = match tokio::time::timeout_at(
                directory_deadline,
                self.refresh(key, location, &prefix),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    report.partial = true;
                    break;
                }
            };
            report.refreshed_directories += 1;
            for child in listing
                .entries
                .iter()
                .filter(|e| matches!(e.kind, crate::store::PathListKind::Directory))
            {
                if seen.contains(&child.path) {
                    continue;
                }
                if seen.len() >= max_directories {
                    report.partial = true;
                    continue;
                }
                seen.insert(child.path.clone());
                queue.push_back(child.path.clone());
            }
            tokio::task::yield_now().await;
        }
        // Publish completeness alongside the cached root, including across restarts.
        // Do not hold the values lock over persistence.
        let root = {
            let mut values = self.values.write().await;
            values.get_mut(key_prefix).map(|root| {
                root.partial = report.partial;
                root.clone()
            })
        };
        if let Some(root) = root {
            if let Err(error) = self
                .disk
                .upsert(&key_prefix.to_owned(), &serde_json::to_value(root)?, &[])
                .await
            {
                tracing::warn!(target: "pchronicle.serve", %error, "mount completeness persistence failed");
            }
        }
        tracing::info!(target: "pchronicle.serve", %key_prefix,
            refreshed = report.refreshed_directories, partial = report.partial,
            "manifest cache refresh finished");
        Ok(report)
    }

    pub fn spawn_periodic_refresh(
        &self,
        key_prefix: String,
        location: DatasetLocation,
        interval: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let cache = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                if let Err(error) = cache.refresh_mount(&key_prefix, &location).await {
                    tracing::warn!(target: "pchronicle.serve", %error, "manifest cache refresh failed");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{PathListEntry, PathListKind};

    #[tokio::test]
    async fn bounded_walk_reports_and_persists_partial_then_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("source");
        std::fs::create_dir_all(root.join("child")).unwrap();
        let location = DatasetLocation::parse(root.to_str().unwrap()).unwrap();
        let path = dir.path().join("cache.lance");
        let cache = ManifestCache::open(path.clone()).await;
        let partial = cache
            .refresh_mount_bounded("mount", &location, 1, Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!(
            partial,
            ManifestRefreshReport {
                refreshed_directories: 1,
                partial: true
            }
        );
        assert!(cache.summary("mount").await.partial);
        drop(cache);
        let cache = ManifestCache::open(path).await;
        assert!(cache.summary("mount").await.partial);
        let complete = cache.refresh_mount("mount", &location).await.unwrap();
        assert!(!complete.partial);
        assert!(!cache.summary("mount").await.partial);
        let timed_out = cache
            .refresh_mount_bounded("mount", &location, 10, Duration::ZERO)
            .await
            .unwrap();
        assert!(timed_out.partial);
        assert_eq!(timed_out.refreshed_directories, 0);
    }

    #[tokio::test]
    async fn refresh_survives_disk_failure_and_unrelated_refresh_lock() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let location = DatasetLocation::parse(source.to_str().unwrap()).unwrap();
        let path = dir.path().join("manifest.lance");
        let cache = ManifestCache::open(path.clone()).await;
        // Inject a persistent disk failure after opening a writable cache.
        std::fs::write(&path, "not a Lance directory").unwrap();
        assert!(
            cache
                .disk
                .upsert(&"probe".into(), &serde_json::Value::Null, &[])
                .await
                .is_err()
        );
        let gate = crate::store::root_write_lock::for_root(
            &serde_json::to_string(&(cache.path(), "blocked")).unwrap(),
        );
        let _guard = gate.lock().await;
        let listing = tokio::time::timeout(
            Duration::from_secs(5),
            cache.refresh("healthy", &location, ""),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!listing.entries.is_empty());
        assert_eq!(
            cache.get("healthy").await.unwrap().entries.len(),
            listing.entries.len()
        );
        cache.refresh_mount("mount", &location).await.unwrap();
        assert!(cache.get("mount").await.is_some());
        assert!(
            cache
                .refresh("missing", &location, "../escape")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn summary_counts_cached_manifest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::open(dir.path().join("manifest.lance")).await;
        cache.values.write().await.insert(
            "mount".into(),
            ManifestListing {
                partial: false,
                entries: vec![PathListEntry {
                    name: "a".into(),
                    path: "a".into(),
                    kind: PathListKind::Dataset,
                    format: None,
                    record_count: Some(3),
                    failed_count: None,
                }],
                observed_at: 0,
            },
        );
        assert_eq!(
            cache.summary("mount").await,
            LocationSummary {
                partial: false,
                datasets: 1,
                trajectories: 3
            }
        );
        cache.values.write().await.insert(
            "mount\0nested".into(),
            ManifestListing {
                partial: false,
                entries: vec![PathListEntry {
                    name: "b".into(),
                    path: "nested/b".into(),
                    kind: PathListKind::File,
                    format: Some("storyline-lance".into()),
                    record_count: Some(4),
                    failed_count: None,
                }],
                observed_at: 0,
            },
        );
        assert_eq!(cache.summary_under("mount").await.trajectories, 7);
    }

    #[tokio::test]
    async fn summary_under_aggregates_slash_descendants_from_cache() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        for (path, count) in [
            ("codex2", 0),
            ("codex3", 0),
            ("nested/nested1/codex2", 354),
            ("nested/nested2/codex2", 354),
        ] {
            let leaf = source.join(path);
            std::fs::create_dir_all(&leaf).unwrap();
            crate::store::catalog::manifest::write_compact_jsonl_manifest(&leaf, 1, count).unwrap();
        }
        let location = DatasetLocation::parse(source.to_str().unwrap()).unwrap();
        let cache_path = temp.path().join("manifest.lance");
        let cache = ManifestCache::open(cache_path.clone()).await;
        let mount = "rfs\0fingerprint";
        cache.refresh_mount(mount, &location).await.unwrap();
        assert!(
            cache
                .get("rfs\0fingerprint\0nested/nested1")
                .await
                .is_some()
        );
        drop(cache);
        std::fs::remove_dir_all(&source).unwrap();

        // No source I/O or projection merging: aggregate persisted observations.
        let cache = ManifestCache::open(cache_path).await;
        for (prefix, datasets, trajectories) in [
            ("rfs\0fingerprint", 4, 708),
            ("rfs\0fingerprint\0nested", 2, 708),
            ("rfs\0fingerprint\0nested/nested1", 1, 354),
            ("rfs\0fingerprint\0missing", 0, 0),
        ] {
            assert_eq!(
                cache.summary_under(prefix).await,
                LocationSummary {
                    partial: false,
                    datasets,
                    trajectories
                },
                "{prefix:?}"
            );
        }
        let distractor = cache.get("rfs\0fingerprint\0nested/nested1").await.unwrap();
        let mut distractor = distractor;
        distractor.entries[0].path = "unrelated/leaf".into();
        distractor.entries[0].record_count = Some(999);
        for key in [
            "rfs\0fingerprint\0nested-other/child",
            "rfs\0different\0nested/child",
            "other\0fingerprint\0nested/child",
        ] {
            cache
                .values
                .write()
                .await
                .insert(key.into(), distractor.clone());
        }
        assert_eq!(
            cache.summary_under("rfs\0fingerprint\0nested").await,
            LocationSummary {
                partial: false,
                datasets: 2,
                trajectories: 708
            }
        );
    }

    #[tokio::test]
    async fn read_mode_uses_cache_then_refreshes_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let dataset = dir.path().join("leaf");
        std::fs::create_dir_all(&dataset).unwrap();
        crate::store::catalog::manifest::write_compact_jsonl_manifest(&dataset, 1, 7).unwrap();
        let location = DatasetLocation::parse(dir.path().to_str().unwrap()).unwrap();
        let cache_path = dir.path().join("cache.lance");
        let cache = ManifestCache::open(cache_path.clone()).await;
        assert!(
            cache
                .get_or_refresh("mount", &location, "", ManifestReadMode::Cached)
                .await
                .unwrap()
                .is_none()
        );
        let listing = cache
            .get_or_refresh("mount", &location, "", ManifestReadMode::RefreshIfMissing)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert!(
            cache
                .get_or_refresh("mount", &location, "", ManifestReadMode::Cached)
                .await
                .unwrap()
                .is_some()
        );
        drop(cache);
        assert!(
            ManifestCache::open(cache_path)
                .await
                .get("mount")
                .await
                .is_some()
        );
    }
}
