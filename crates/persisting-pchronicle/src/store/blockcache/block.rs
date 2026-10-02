use super::config::CacheConfig;
use bytes::Bytes;
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub hit_bytes: u64,
    pub downloaded_bytes: u64,
    pub coalesced_reads: u64,
    pub write_errors: u64,
    pub resident_bytes: u64,
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    hit_bytes: AtomicU64,
    downloaded_bytes: AtomicU64,
    coalesced_reads: AtomicU64,
    write_errors: AtomicU64,
    resident_bytes: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct BlockCache {
    config: CacheConfig,
    shared: Arc<Shared>,
}

#[derive(Debug, Default)]
struct Shared {
    counters: Counters,
    // ponytail: downloads coalesce within a process; add per-block file locks
    // if multiple worker processes contend on the same cold working set.
    flights: tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    last_trim_ms: AtomicU64,
    trim_lock: tokio::sync::Mutex<()>,
    accesses: Mutex<HashMap<PathBuf, std::time::SystemTime>>,
    writes_since_trim: AtomicU64,
}

impl BlockCache {
    pub fn new(config: CacheConfig) -> Self {
        static CACHES: OnceLock<Mutex<HashMap<CacheConfig, Arc<Shared>>>> = OnceLock::new();
        let mut caches = CACHES
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // ponytail: retain 64 cache configurations per process; use an LRU of
        // configurations if a service actually rotates through more roots.
        if !caches.contains_key(&config)
            && caches.len() >= 64
            && let Some(key) = caches.keys().next().cloned()
        {
            caches.remove(&key);
        }
        let shared = caches.entry(config.clone()).or_default().clone();
        Self { config, shared }
    }
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.shared.counters.hits.load(Ordering::Relaxed),
            misses: self.shared.counters.misses.load(Ordering::Relaxed),
            evictions: self.shared.counters.evictions.load(Ordering::Relaxed),
            hit_bytes: self.shared.counters.hit_bytes.load(Ordering::Relaxed),
            downloaded_bytes: self
                .shared
                .counters
                .downloaded_bytes
                .load(Ordering::Relaxed),
            coalesced_reads: self.shared.counters.coalesced_reads.load(Ordering::Relaxed),
            write_errors: self.shared.counters.write_errors.load(Ordering::Relaxed),
            resident_bytes: self.shared.counters.resident_bytes.load(Ordering::Relaxed),
        }
    }
    pub fn block_path(&self, key: &str, block: u64) -> PathBuf {
        let hash = blake3::hash(format!("{key}:{block}").as_bytes())
            .to_hex()
            .to_string();
        // Keep directory fan-out bounded; a single flat cache directory becomes
        // expensive once a service has browsed many datasets and versions.
        self.config
            .root
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash)
    }
    pub async fn get_or_fetch<F>(
        &self,
        path: &Path,
        expected: usize,
        fetch: F,
    ) -> object_store::Result<Bytes>
    where
        F: Future<Output = object_store::Result<Bytes>>,
    {
        match tokio::fs::read(path).await {
            Ok(bytes) if bytes.len() == expected => {
                self.record_hit(path, bytes.len()).await;
                // Refresh recency before restart cleanup can choose victims.
                self.schedule_trim();
                return Ok(Bytes::from(bytes));
            }
            _ => self.schedule_trim(),
        };
        let flight = {
            let mut flights = self.shared.flights.lock().await;
            // Cancelled fetches leave only the map's Arc; reclaim them on the
            // next miss rather than retaining every cancelled object path.
            flights.retain(|_, flight| Arc::strong_count(flight) > 1);
            flights
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let flight_guard = flight.lock().await;
        let result = async {
            if let Ok(bytes) = tokio::fs::read(path).await
                && bytes.len() == expected
            {
                self.shared.counters.coalesced_reads.fetch_add(1, Ordering::Relaxed);
                self.record_hit(path, bytes.len()).await;
                return Ok(Bytes::from(bytes));
            }
            self.shared.counters.misses.fetch_add(1, Ordering::Relaxed);
            let bytes = fetch.await?;
            if bytes.len() != expected {
                return Err(object_store::Error::Generic {
                    store: "pchronicle-cache",
                    source: format!("block length {}, expected {expected}", bytes.len()).into(),
                });
            }
            self.shared.counters.downloaded_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            tracing::debug!(target: "pchronicle.block_cache", block = %path.display(), bytes = bytes.len(), "block cache miss");
            // A block larger than the entire budget would evict useful data and
            // immediately evict itself. Serve it without polluting the cache.
            if expected as u64 <= self.config.capacity_bytes {
                let tmp = path.with_extension(format!("{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4()));
                let write = async {
                    tokio::fs::create_dir_all(path.parent().unwrap_or(&self.config.root)).await?;
                    tokio::fs::write(&tmp, &bytes).await?;
                    tokio::fs::rename(&tmp, path).await
                }.await;
                match write {
                    Ok(()) => {
                        self.accesses().insert(path.to_path_buf(), std::time::SystemTime::now());
                        self.shared.writes_since_trim.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        self.schedule_trim();
                    }
                    Err(error) => {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        self.shared.counters.write_errors.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(target: "pchronicle.block_cache", %error, "cache write failed; serving downloaded bytes");
                    }
                }
            }
            Ok(bytes)
        }
        .await;
        drop(flight_guard);
        let mut flights = self.shared.flights.lock().await;
        if Arc::strong_count(&flight) == 2
            && flights
                .get(path)
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            flights.remove(path);
        }
        result
    }
    fn accesses(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, std::time::SystemTime>> {
        self.shared
            .accesses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn record_hit(&self, path: &Path, size: usize) {
        self.shared.counters.hits.fetch_add(1, Ordering::Relaxed);
        self.shared
            .counters
            .hit_bytes
            .fetch_add(size as u64, Ordering::Relaxed);
        let now = std::time::SystemTime::now();
        self.accesses().insert(path.to_path_buf(), now);
        tracing::debug!(target: "pchronicle.block_cache", block = %path.display(), bytes = size, "block cache hit");
        // Persist recency so the hot working set survives worker restarts.
        let path = path.to_path_buf();
        let _ = tokio::task::spawn_blocking(move || {
            std::fs::File::options()
                .write(true)
                .open(path)?
                .set_times(std::fs::FileTimes::new().set_modified(now))
        })
        .await;
    }

    fn schedule_trim(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        let last = self.shared.last_trim_ms.load(Ordering::Relaxed);
        let full = self.shared.writes_since_trim.load(Ordering::Relaxed)
            >= (self.config.capacity_bytes / 10).max(1);
        if (!full && now.saturating_sub(last) < 5_000)
            || self
                .shared
                .last_trim_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move {
            let Ok(_guard) = cache.shared.trim_lock.try_lock() else {
                return;
            };
            cache.trim_inner().await;
        });
    }
    pub async fn trim(&self) {
        let _guard = self.shared.trim_lock.lock().await;
        self.trim_inner().await;
    }

    async fn trim_inner(&self) {
        self.shared.writes_since_trim.swap(0, Ordering::Relaxed);
        // ponytail: reconcile by scanning disk, including other workers' files;
        // use a persistent block index if millions of blocks make this costly.
        let scan_started = std::time::SystemTime::now();
        let mut files = Vec::new();
        let mut total = 0;
        let mut dirs = vec![self.config.root.clone()];
        while let Some(dir) = dirs.pop() {
            let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
                continue;
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if let Ok(meta) = entry.metadata().await {
                    if meta.file_type().is_symlink() {
                        continue;
                    }
                    if meta.is_dir() {
                        dirs.push(path);
                    } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "tmp") {
                        let owned =
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .is_some_and(|name| {
                                    name.get(..64).is_some_and(|hash| {
                                        hash.bytes().all(|c| c.is_ascii_hexdigit())
                                    }) && name.as_bytes().get(64) == Some(&b'.')
                                });
                        if owned
                            && meta
                                .modified()
                                .ok()
                                .and_then(|time| time.elapsed().ok())
                                .is_some_and(|age| age > std::time::Duration::from_secs(300))
                        {
                            let _ = tokio::fs::remove_file(path).await;
                        }
                    } else if meta.is_file() {
                        total += meta.len();
                        let modified = meta.modified().ok();
                        let recent = self.accesses().get(&path).copied().max(modified);
                        files.push((recent, modified, meta.len(), path));
                    }
                }
            }
        }
        let present = files
            .iter()
            .map(|(_, _, _, path)| path.clone())
            .collect::<std::collections::HashSet<_>>();
        self.accesses()
            .retain(|path, time| present.contains(path) || *time >= scan_started);
        files.sort_by_key(|(recent, _, _, _)| *recent);
        // Evict at 90% to an 80% target, leaving space for the next downloads.
        let reserve = self.config.capacity_bytes / 10;
        let target = if total >= self.config.capacity_bytes - reserve {
            self.config.capacity_bytes - 2 * reserve
        } else {
            self.config.capacity_bytes
        };
        for (recent, modified, size, path) in files {
            if total <= target {
                break;
            }
            if self.shared.flights.lock().await.contains_key(&path) {
                continue;
            }
            // Another reader/process may have refreshed this block since the
            // scan; do not delete its new working set based on an old snapshot.
            if self
                .accesses()
                .get(&path)
                .copied()
                .is_some_and(|now| Some(now) > recent)
                || tokio::fs::metadata(&path)
                    .await
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    > modified
            {
                continue;
            }
            if tokio::fs::remove_file(&path).await.is_ok() {
                self.accesses().remove(&path);
                total -= size;
                self.shared
                    .counters
                    .evictions
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        self.shared
            .counters
            .resident_bytes
            .store(total, Ordering::Relaxed);
        tracing::debug!(target: "pchronicle.block_cache", root = %self.config.root.display(), capacity_bytes = self.config.capacity_bytes, stats = ?self.stats(), "block cache status");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lru_survives_losing_the_in_memory_recency() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(CacheConfig::new(dir.path().into(), 8, 4));
        cache.shared.last_trim_ms.store(u64::MAX, Ordering::Relaxed);
        let hot = dir.path().join("hot");
        let cold = dir.path().join("cold");
        for (path, age) in [(&hot, 1), (&cold, 2)] {
            std::fs::write(path, b"data").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(age)),
                )
                .unwrap();
        }
        cache
            .get_or_fetch(&hot, 4, async { panic!("hot block must hit") })
            .await
            .unwrap();
        let fresh = dir.path().join("fresh");
        std::fs::write(&fresh, b"data").unwrap();
        // Simulate a restart: only the on-disk access times remain.
        cache.accesses().clear();
        cache.trim().await;
        assert!(hot.exists());
        assert!(fresh.exists());
        assert!(!cold.exists());
        assert_eq!(cache.stats().resident_bytes, 8);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[tokio::test]
    async fn high_watermark_eviction_leaves_room_before_capacity_is_exceeded() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(CacheConfig::new(dir.path().into(), 100, 30));
        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(name), [0; 30]).unwrap();
        }
        cache.trim().await;
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().resident_bytes, 60);
    }

    #[tokio::test]
    async fn independently_opened_caches_coalesce_downloads_and_share_stats() {
        let dir = tempfile::tempdir().unwrap();
        let config = CacheConfig::new(dir.path().into(), 1024, 4);
        let first = BlockCache::new(config.clone());
        let second = BlockCache::new(config);
        let path = first.block_path("object", 0);
        let downloads = AtomicU64::new(0);
        let fetch = || async {
            downloads.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            Ok(Bytes::from_static(b"data"))
        };
        let (a, b) = tokio::join!(
            first.get_or_fetch(&path, 4, fetch()),
            second.get_or_fetch(&path, 4, fetch())
        );
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(downloads.load(Ordering::Relaxed), 1);
        assert_eq!(first.stats().misses, 1);
        assert_eq!(second.stats().downloaded_bytes, 4);
        assert_eq!(second.stats().hits, 1);
    }

    #[tokio::test]
    async fn cache_write_failure_and_oversized_blocks_do_not_fail_reads() {
        let blocked = tempfile::NamedTempFile::new().unwrap();
        let cache = BlockCache::new(CacheConfig::new(blocked.path().into(), 1024, 4));
        let path = cache.block_path("object", 0);
        assert_eq!(
            cache
                .get_or_fetch(&path, 4, async { Ok(Bytes::from_static(b"data")) })
                .await
                .unwrap(),
            Bytes::from_static(b"data")
        );
        assert_eq!(cache.stats().write_errors, 1);
        let dir = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(CacheConfig::new(dir.path().into(), 2, 4));
        let path = cache.block_path("oversized", 0);
        cache
            .get_or_fetch(&path, 4, async { Ok(Bytes::from_static(b"data")) })
            .await
            .unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn hit_miss_and_corrupt_are_observable() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlockCache::new(CacheConfig::new(dir.path().into(), 100, 4));
        let p = dir.path().join("x");
        assert_eq!(
            cache
                .get_or_fetch(&p, 4, async { Ok(Bytes::from_static(b"abcd")) })
                .await
                .unwrap(),
            Bytes::from_static(b"abcd")
        );
        assert_eq!(
            cache
                .get_or_fetch(&p, 4, async { panic!("cache miss") })
                .await
                .unwrap(),
            Bytes::from_static(b"abcd")
        );
        tokio::fs::write(&p, b"bad").await.unwrap();
        assert_eq!(
            cache
                .get_or_fetch(&p, 4, async { Ok(Bytes::from_static(b"efgh")) })
                .await
                .unwrap(),
            Bytes::from_static(b"efgh")
        );
        let mut stats = cache.stats();
        stats.resident_bytes = 0; // Background reconciliation is asynchronous.
        assert_eq!(
            stats,
            CacheStats {
                hits: 1,
                misses: 2,
                evictions: 0,
                hit_bytes: 4,
                downloaded_bytes: 8,
                ..Default::default()
            }
        );
    }
}
