//! Bound and coalesce UI summary queries. Cache successful summaries (not query
//! engines or source pins), returning stale results while one refresh runs.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use persisting_pchronicle::storage::QueryScope;
use tokio::sync::{OnceCell, Semaphore};

use super::RunSummary;
use super::acceleration::SharedAccelerationFailure;

type Summaries = Arc<Vec<RunSummary>>;
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const MAX_CACHE_ENTRIES: usize = 32;
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
// One background summary refresh per process, with no waiting task queue.
pub(super) static REFRESH_SLOT: Semaphore = Semaphore::const_new(1);

struct CachedSummaries {
    summaries: Summaries,
    built_at: Instant,
    refresh_after: Instant,
    last_used: Instant,
    bytes: usize,
}

#[derive(Default)]
struct SummaryCache {
    generation: u64,
    entries: HashMap<QueryScope, CachedSummaries>,
}

fn summary_bytes(summaries: &Vec<RunSummary>) -> usize {
    summaries.capacity() * std::mem::size_of::<RunSummary>()
        + summaries
            .iter()
            .map(|run| {
                run.dataset.capacity()
                    + run.file.capacity()
                    + run.document_id.capacity()
                    + run.agent_id.capacity()
                    + run.session_id.capacity()
                    + run.path.capacity()
                    + run.status.capacity()
                    + [
                        &run.run_id,
                        &run.model_name,
                        &run.root_session_id,
                        &run.format,
                    ]
                    .into_iter()
                    .flatten()
                    .map(String::capacity)
                    .sum::<usize>()
            })
            .sum::<usize>()
}

#[derive(Default)]
struct Flight {
    result: OnceCell<Result<Summaries, SharedAccelerationFailure>>,
}

pub(super) struct ScopedQueries {
    flights: Mutex<HashMap<QueryScope, Weak<Flight>>>,
    slots: Semaphore,
    cache: Mutex<SummaryCache>,
}

impl Default for ScopedQueries {
    fn default() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            slots: Semaphore::new(2),
            cache: Mutex::new(SummaryCache::default()),
        }
    }
}

impl ScopedQueries {
    pub(super) fn invalidate(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.generation += 1;
        cache.entries.clear();
        // Requests started after explicit refresh must not join an older build.
        self.flights.lock().unwrap().clear();
    }

    fn publish(&self, scope: QueryScope, generation: u64, summaries: Summaries) {
        let bytes = summary_bytes(&summaries);
        let mut cache = self.cache.lock().unwrap();
        if cache.generation != generation {
            return;
        }
        cache.entries.remove(&scope);
        if bytes > MAX_CACHE_BYTES {
            return;
        }
        while cache.entries.len() >= MAX_CACHE_ENTRIES
            || cache
                .entries
                .values()
                .map(|entry| entry.bytes)
                .sum::<usize>()
                + bytes
                > MAX_CACHE_BYTES
        {
            let oldest = cache
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
                .unwrap();
            cache.entries.remove(&oldest);
        }
        let now = Instant::now();
        cache.entries.insert(
            scope,
            CachedSummaries {
                summaries,
                built_at: now,
                refresh_after: now + REFRESH_INTERVAL,
                last_used: now,
                bytes,
            },
        );
    }

    pub(super) async fn cached<F, Fut>(
        self: &Arc<Self>,
        scope: QueryScope,
        execute: F,
    ) -> Result<(Summaries, &'static str)>
    where
        F: FnOnce(bool) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Summaries>> + Send + 'static,
    {
        let (generation, cached) = {
            let mut cache = self.cache.lock().unwrap();
            let generation = cache.generation;
            let cached = cache.entries.get_mut(&scope).map(|entry| {
                entry.last_used = Instant::now();
                let stale = entry.built_at.elapsed() >= REFRESH_INTERVAL;
                let permit = (stale && entry.last_used >= entry.refresh_after)
                    .then(|| REFRESH_SLOT.try_acquire().ok())
                    .flatten();
                if permit.is_some() {
                    entry.refresh_after = Instant::now() + REFRESH_INTERVAL;
                }
                (entry.summaries.clone(), stale, permit)
            });
            (generation, cached)
        };
        if let Some((summaries, stale, permit)) = cached {
            if let Some(permit) = permit {
                let queries = self.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    match queries.run(scope.clone(), || execute(true)).await {
                        Ok(summaries) => queries.publish(scope, generation, summaries),
                        Err(error) => {
                            let mut cache = queries.cache.lock().unwrap();
                            if cache.generation == generation
                                && let Some(entry) = cache.entries.get_mut(&scope)
                            {
                                entry.refresh_after = Instant::now() + REFRESH_INTERVAL;
                            }
                            tracing::warn!(target: super::LOG_TARGET, dataset = %scope.dataset,
                                error = %error, "UI run summary refresh failed; retaining cached results");
                        }
                    }
                });
            }
            return Ok((
                summaries,
                if stale {
                    "summary_cache_stale"
                } else {
                    "summary_cache_hit"
                },
            ));
        }
        let summaries = self.run(scope.clone(), || execute(false)).await?;
        self.publish(scope, generation, summaries.clone());
        Ok((summaries, "summary_cache_miss"))
    }

    pub(super) async fn run<F, Fut>(&self, scope: QueryScope, execute: F) -> Result<Summaries>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Summaries>>,
    {
        let flight = {
            let mut flights = self.flights.lock().unwrap();
            flights.retain(|_, entry| entry.strong_count() > 0);
            if let Some(flight) = flights.get(&scope).and_then(Weak::upgrade) {
                flight
            } else {
                anyhow::ensure!(flights.len() < 128, "scoped query admission queue full");
                let flight = Arc::new(Flight::default());
                flights.insert(scope, Arc::downgrade(&flight));
                flight
            }
        };
        // OnceCell transfers initialization to a waiter if the initializing
        // request is cancelled. No detached tasks or permanently owned pins.
        flight
            .result
            .get_or_init(|| async {
                let _slot = self.slots.acquire().await.expect("admission never closes");
                execute().await.map_err(SharedAccelerationFailure::new)
            })
            .await
            .clone()
            .map_err(anyhow::Error::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn scope(name: &str) -> QueryScope {
        QueryScope {
            dataset: name.into(),
            source_file: None,
        }
    }

    fn expire(queries: &ScopedQueries, key: &QueryScope) {
        let mut cache = queries.cache.lock().unwrap();
        let entry = cache.entries.get_mut(key).unwrap();
        entry.built_at = Instant::now() - REFRESH_INTERVAL;
        entry.refresh_after = Instant::now();
    }

    #[tokio::test]
    async fn ui_cache_reuses_results_bounds_refresh_and_retains_failures() {
        let queries = Arc::new(ScopedQueries::default());
        let initial = Arc::new(Vec::new());
        let value = initial.clone();
        let (first, status) = queries
            .cached(scope("a"), |_| async { Ok(value) })
            .await
            .unwrap();
        assert_eq!(status, "summary_cache_miss");
        let (hit, status) = queries
            .cached(scope("a"), |_| async { panic!("cache hit must not scan") })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &hit));
        assert_eq!(status, "summary_cache_hit");
        queries
            .cached(scope("b"), |_| async { Ok(Arc::new(Vec::new())) })
            .await
            .unwrap();
        expire(&queries, &scope("a"));
        expire(&queries, &scope("b"));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, wait) = tokio::sync::oneshot::channel();
        let updated = Arc::new(Vec::new());
        let value = updated.clone();
        let (stale, status) = queries
            .cached(scope("a"), |_| async move {
                started.send(()).unwrap();
                wait.await.unwrap();
                Ok(value)
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&initial, &stale));
        assert_eq!(status, "summary_cache_stale");
        ready.await.unwrap();
        for name in ["a", "b"] {
            let (_, status) = queries
                .cached(scope(name), |_| async {
                    panic!("only one background refresh")
                })
                .await
                .unwrap();
            assert_eq!(status, "summary_cache_stale");
        }
        finish.send(()).unwrap();
        let permit = REFRESH_SLOT.acquire().await.unwrap();
        assert!(Arc::ptr_eq(
            &queries.cache.lock().unwrap().entries[&scope("a")].summaries,
            &updated
        ));
        drop(permit);

        expire(&queries, &scope("a"));
        let (started, ready) = tokio::sync::oneshot::channel();
        queries
            .cached(scope("a"), |_| async move {
                started.send(()).unwrap();
                Err(anyhow::anyhow!("S3 unavailable"))
            })
            .await
            .unwrap();
        ready.await.unwrap();
        let permit = REFRESH_SLOT.acquire().await.unwrap();
        drop(permit);
        let (retained, status) = queries
            .cached(scope("a"), |_| async { panic!("retry must back off") })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&retained, &updated));
        assert_eq!(status, "summary_cache_stale");

        // Explicit refresh invalidates results and prevents a late background
        // completion from republishing data from the previous generation.
        expire(&queries, &scope("a"));
        let (started, ready) = tokio::sync::oneshot::channel();
        let (finish, wait) = tokio::sync::oneshot::channel();
        queries
            .cached(scope("a"), |_| async move {
                started.send(()).unwrap();
                wait.await.unwrap();
                Ok(Arc::new(Vec::new()))
            })
            .await
            .unwrap();
        ready.await.unwrap();
        queries.invalidate();
        finish.send(()).unwrap();
        let _permit = REFRESH_SLOT.acquire().await.unwrap();
        assert!(queries.cache.lock().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn ui_cache_bounds_retention_and_never_caches_cold_failures() {
        let queries = Arc::new(ScopedQueries::default());
        assert!(
            queries
                .cached(scope("a"), |_| async {
                    Err(anyhow::anyhow!("unavailable"))
                })
                .await
                .is_err()
        );
        assert!(queries.cache.lock().unwrap().entries.is_empty());
        for n in 0..=MAX_CACHE_ENTRIES {
            queries
                .cached(scope(&n.to_string()), |_| async {
                    Ok(Arc::new(Vec::new()))
                })
                .await
                .unwrap();
        }
        let cache = queries.cache.lock().unwrap();
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        assert!(!cache.entries.contains_key(&scope("0")));
        drop(cache);
        // Account for allocated vector capacity, not just serialized row bytes.
        let oversized = Vec::with_capacity(MAX_CACHE_BYTES / std::mem::size_of::<RunSummary>() + 1);
        queries.publish(scope("oversized"), 0, Arc::new(oversized));
        assert!(
            !queries
                .cache
                .lock()
                .unwrap()
                .entries
                .contains_key(&scope("oversized"))
        );
    }

    #[tokio::test]
    async fn overlapping_requests_share_work_but_later_request_repins() {
        let queries = ScopedQueries::default();
        let builds = AtomicUsize::new(0);
        let execute = || async {
            builds.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(Arc::new(Vec::new()))
        };
        let (a, b) = tokio::join!(
            queries.run(scope("a"), execute),
            queries.run(scope("a"), execute)
        );
        assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        queries.run(scope("a"), execute).await.unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        assert!(
            queries
                .flights
                .lock()
                .unwrap()
                .values()
                .all(|entry| entry.upgrade().is_none())
        );
    }

    #[tokio::test]
    async fn execution_admission_limits_distinct_scopes_until_queries_finish() {
        let queries = ScopedQueries::default();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let execute = || async {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(Arc::new(Vec::new()))
        };
        let (a, b, c) = tokio::join!(
            queries.run(scope("a"), execute),
            queries.run(scope("b"), execute),
            queries.run(scope("c"), execute)
        );
        a.unwrap();
        b.unwrap();
        c.unwrap();
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(queries.slots.available_permits(), 2);
    }

    #[tokio::test]
    async fn shared_failure_preserves_cause_and_next_request_retries() {
        let queries = ScopedQueries::default();
        let builds = AtomicUsize::new(0);
        let execute = || async {
            builds.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
            Err(anyhow::anyhow!("source unavailable").context("pin query source"))
        };
        let (a, b) = tokio::join!(
            queries.run(scope("a"), execute),
            queries.run(scope("a"), execute)
        );
        for result in [a, b] {
            let error = result.unwrap_err();
            assert_eq!(error.root_cause().to_string(), "source unavailable");
            assert!(format!("{error:#}").contains("pin query source"));
        }
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        queries
            .run(scope("a"), || async { Ok(Arc::new(Vec::new())) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_initializer_releases_admission_and_waiter_takes_over() {
        let queries = ScopedQueries::default();
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut first = Box::pin(queries.run(scope("a"), || async {
            started.send(()).unwrap();
            std::future::pending::<Result<Summaries>>().await
        }));
        tokio::select! { _ = &mut first => panic!("unexpected completion"), _ = ready => {} }
        let mut waiter = Box::pin(queries.run(scope("a"), || async { Ok(Arc::new(Vec::new())) }));
        tokio::select! { biased; _ = &mut waiter => panic!("must wait"), _ = std::future::ready(()) => {} }
        drop(first);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(queries.slots.available_permits(), 2);
    }
}
