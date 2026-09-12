//! Coalesce overlapping scoped summary requests, without caching pinned sources
//! across independent requests. Admission covers discovery AND query execution.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, Weak};

use anyhow::Result;
use persisting_pchronicle::storage::QueryScope;
use tokio::sync::{OnceCell, Semaphore};

use super::RunSummary;
use super::acceleration::SharedAccelerationFailure;

type Summaries = Arc<Vec<RunSummary>>;
#[derive(Default)]
struct Flight {
    result: OnceCell<Result<Summaries, SharedAccelerationFailure>>,
}

pub(super) struct ScopedQueries {
    flights: Mutex<HashMap<QueryScope, Weak<Flight>>>,
    slots: Semaphore,
}

impl Default for ScopedQueries {
    fn default() -> Self {
        Self {
            flights: Mutex::new(HashMap::new()),
            slots: Semaphore::new(2),
        }
    }
}

impl ScopedQueries {
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
