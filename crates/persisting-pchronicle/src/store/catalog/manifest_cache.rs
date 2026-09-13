//! Rebuildable cache of manifest-derived directory observations.
//!
//! This module deliberately knows nothing about DataFusion.  It is the single
//! boundary used by navigational callers that can tolerate a stale snapshot.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::sync::RwLock;

use crate::store::{DatasetLocation, PathListEntry, PersistentCache};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestListing {
    pub entries: Vec<PathListEntry>,
    pub observed_at: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationSummary {
    pub datasets: u64,
    pub trajectories: u64,
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
    refresh_gate: Arc<Mutex<()>>,
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
        Self {
            disk,
            values,
            refresh_gate: Arc::new(Mutex::new(())),
        }
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
        let _guard = self.refresh_gate.lock().await;
        let key = key.into();
        let listing = ManifestListing {
            entries: location.list(prefix).await?,
            observed_at: chrono::Utc::now().timestamp(),
        };
        self.values
            .write()
            .await
            .insert(key.clone(), listing.clone());
        self.disk
            .upsert(&key, &serde_json::to_value(&listing)?, &[])
            .await?;
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
        let values = self.values.read().await;
        values
            .iter()
            .filter(|(key, _)| key == &key_prefix || key.starts_with(&format!("{key_prefix}\0")))
            .fold(LocationSummary::default(), |mut total, (_, listing)| {
                for entry in &listing.entries {
                    if matches!(entry.kind, crate::store::PathListKind::Dataset) {
                        total.datasets += 1;
                        total.trajectories += entry.record_count.unwrap_or_default();
                    }
                }
                total
            })
    }

    /// Breadth-first refresh of a mount. Only one refresh runs at a time;
    /// shallow paths are published before deeper paths.
    pub async fn refresh_mount(&self, key_prefix: &str, location: &DatasetLocation) -> Result<()> {
        let _guard = self.refresh_gate.lock().await;
        let mut queue = VecDeque::from([String::new()]);
        while let Some(prefix) = queue.pop_front() {
            let listing = ManifestListing {
                entries: location.list(&prefix).await?,
                observed_at: chrono::Utc::now().timestamp(),
            };
            let key = if prefix.is_empty() {
                key_prefix.to_owned()
            } else {
                format!("{key_prefix}\0{prefix}")
            };
            self.values
                .write()
                .await
                .insert(key.clone(), listing.clone());
            self.disk
                .upsert(&key, &serde_json::to_value(&listing)?, &[])
                .await?;
            for child in listing
                .entries
                .iter()
                .filter(|e| matches!(e.kind, crate::store::PathListKind::Directory))
            {
                queue.push_back(child.path.clone());
            }
            tokio::task::yield_now().await;
        }
        Ok(())
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
    async fn summary_counts_cached_manifest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ManifestCache::open(dir.path().join("manifest.lance")).await;
        cache.values.write().await.insert(
            "mount".into(),
            ManifestListing {
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
                datasets: 1,
                trajectories: 3
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
