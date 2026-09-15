use super::config::CacheConfig;
use bytes::Bytes;
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[derive(Debug, Default)]
struct Counters {
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct BlockCache {
    config: CacheConfig,
    counters: Arc<Counters>,
    flights: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
    last_trim_ms: Arc<AtomicU64>,
}

impl BlockCache {
    pub fn new(config: CacheConfig) -> Self {
        Self {
            config,
            counters: Arc::new(Counters::default()),
            flights: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            last_trim_ms: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.counters.hits.load(Ordering::Relaxed),
            misses: self.counters.misses.load(Ordering::Relaxed),
            evictions: self.counters.evictions.load(Ordering::Relaxed),
        }
    }
    pub fn block_path(&self, key: &str, block: u64) -> PathBuf {
        self.config.root.join(
            blake3::hash(format!("{key}:{block}").as_bytes())
                .to_hex()
                .to_string(),
        )
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
                self.counters.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(Bytes::from(bytes));
            }
            _ => {
                self.counters.misses.fetch_add(1, Ordering::Relaxed);
            }
        };
        let flight = {
            let mut flights = self.flights.lock().await;
            flights
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let flight_guard = flight.lock().await;
        let result = async {
            if let Ok(bytes) = tokio::fs::read(path).await {
                if bytes.len() == expected {
                    self.counters.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(Bytes::from(bytes));
                }
            }
            let bytes = fetch.await?;
            if bytes.len() != expected {
                return Err(object_store::Error::Generic {
                    store: "pchronicle-cache",
                    source: format!("block length {}, expected {expected}", bytes.len()).into(),
                });
            }
            if tokio::fs::create_dir_all(&self.config.root).await.is_ok() {
                let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
                if tokio::fs::write(&tmp, &bytes).await.is_ok() {
                    let _ = tokio::fs::rename(&tmp, path).await;
                    self.schedule_trim();
                }
            }
            Ok(bytes)
        }
        .await;
        drop(flight_guard);
        let mut flights = self.flights.lock().await;
        if Arc::strong_count(&flight) == 2
            && flights
                .get(path)
                .is_some_and(|current| Arc::ptr_eq(current, &flight))
        {
            flights.remove(path);
        }
        result
    }
    fn schedule_trim(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);
        let last = self.last_trim_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 5_000
            || self
                .last_trim_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let cache = self.clone();
        tokio::spawn(async move { cache.trim().await });
    }
    pub async fn trim(&self) {
        let Ok(mut entries) = tokio::fs::read_dir(&self.config.root).await else {
            return;
        };
        let mut files = Vec::new();
        let mut total = 0;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                if meta.is_file() {
                    total += meta.len();
                    files.push((meta.modified().ok(), meta.len(), entry.path()));
                }
            }
        }
        files.sort_by_key(|(mtime, _, _)| *mtime);
        for (_, size, path) in files {
            if total <= self.config.capacity_bytes {
                break;
            }
            if tokio::fs::remove_file(path).await.is_ok() {
                total -= size;
                self.counters.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(
            cache.stats(),
            CacheStats {
                hits: 1,
                misses: 2,
                evictions: 0
            }
        );
    }
}
