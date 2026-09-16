//! Bounded, opt-in request diagnostics. The observer token is a capability,
//! separate from catalog credentials, so even authentication failures are visible.
use super::problem::ApiError;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const OBSERVER_HEADER: &str = "x-pchronicle-observer";
const LIMIT: usize = 512;
const TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub state: String,
    pub elapsed_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub state: String,
    pub elapsed_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub note: Option<String>,
    pub phases: Vec<Phase>,
    pub worker: Option<Box<Snapshot>>,
}
struct Running {
    snapshot: Snapshot,
    started: Instant,
    phase_started: Instant,
    worker_updated: Instant,
}
#[derive(Clone)]
pub struct Progress(Arc<Mutex<Running>>);
#[derive(Default)]
pub struct Registry(Mutex<HashMap<(String, String), Progress>>);

tokio::task_local! { static CURRENT: Progress; }
pub async fn scope<F: Future>(progress: Progress, work: F) -> F::Output {
    CURRENT.scope(progress, work).await
}
pub fn current() -> Option<Progress> {
    CURRENT.try_with(Clone::clone).ok()
}
pub fn phase(name: &str) {
    if let Some(p) = current() {
        p.phase(name);
    }
}
fn ms(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}

impl Progress {
    pub fn new(id: String, method: String, path: String, worker: bool) -> Self {
        let mut names = if worker {
            vec!["execution"]
        } else {
            vec![
                "authentication",
                "worker_queue",
                "worker_start",
                "worker_execution",
            ]
        };
        if path.ends_with("/explorer/tree") {
            names.extend(["browse_cache", "directory_wait", "manifest_summary"]);
        } else {
            names.extend(["source_metadata", "storage_read", "query"]);
        }
        names.push("response");
        let now = Instant::now();
        Self(Arc::new(Mutex::new(Running {
            snapshot: Snapshot {
                request_id: id,
                method,
                path,
                state: "running".into(),
                elapsed_ms: 0,
                status: None,
                error: None,
                note: None,
                phases: names
                    .into_iter()
                    .map(|name| Phase {
                        name: name.into(),
                        state: "pending".into(),
                        elapsed_ms: 0,
                    })
                    .collect(),
                worker: None,
            },
            started: now,
            phase_started: now,
            worker_updated: now,
        })))
    }
    pub fn note(&self, note: &str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .note = Some(note.into());
    }
    pub fn phase(&self, name: &str) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if v.snapshot.state != "running" {
            return;
        }
        if v.snapshot
            .phases
            .iter()
            .any(|p| p.name == name && p.state == "running")
        {
            return;
        }
        let elapsed = ms(v.phase_started.elapsed());
        for p in &mut v.snapshot.phases {
            if p.state == "running" {
                p.state = "completed".into();
                p.elapsed_ms += elapsed;
            }
        }
        if let Some(p) = v.snapshot.phases.iter_mut().find(|p| p.name == name) {
            p.state = "running".into();
        } else {
            v.snapshot.phases.push(Phase {
                name: name.into(),
                state: "running".into(),
                elapsed_ms: 0,
            });
        }
        v.phase_started = Instant::now();
    }
    pub fn snapshot(&self) -> Snapshot {
        let v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut s = v.snapshot.clone();
        if s.state == "running" {
            s.elapsed_ms = ms(v.started.elapsed());
            for p in &mut s.phases {
                if p.state == "running" {
                    p.elapsed_ms += ms(v.phase_started.elapsed());
                }
            }
            if let Some(w) = s.worker.as_mut() {
                if w.state == "running" {
                    let elapsed = ms(v.worker_updated.elapsed());
                    w.elapsed_ms += elapsed;
                    for p in &mut w.phases {
                        if p.state == "running" {
                            p.elapsed_ms += elapsed;
                        }
                    }
                }
            }
        }
        s
    }
    pub fn worker(&self, snapshot: Snapshot) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        v.snapshot.worker = Some(Box::new(snapshot));
        v.worker_updated = Instant::now();
    }
    pub fn finish(&self, status: Option<u16>, error: Option<String>) {
        let mut v = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if v.snapshot.state != "running" {
            return;
        }
        let state = if status.is_none() {
            "cancelled"
        } else if status.unwrap() >= 400 || error.is_some() {
            "failed"
        } else {
            "completed"
        };
        let elapsed = ms(v.phase_started.elapsed());
        for p in &mut v.snapshot.phases {
            if p.state == "running" {
                p.state = state.into();
                p.elapsed_ms += elapsed;
            } else if p.state == "pending" {
                p.state = "skipped".into();
            }
        }
        if let Some(w) = v.snapshot.worker.as_mut() {
            if w.state == "running" && state != "completed" {
                w.state = state.into();
                for p in &mut w.phases {
                    if p.state == "running" {
                        p.state = state.into();
                    }
                }
            }
        }
        v.snapshot.state = state.into();
        v.snapshot.elapsed_ms = ms(v.started.elapsed());
        v.snapshot.status = status;
        v.snapshot.error = error;
    }
}
pub struct CancelOnDrop(pub Progress);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0
            .finish(None, Some("Request execution was cancelled".into()));
    }
}

fn observer(headers: &HeaderMap) -> Option<String> {
    let token = headers.get(OBSERVER_HEADER)?.to_str().ok()?;
    if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(blake3::hash(token.as_bytes()).to_hex().to_string())
}
impl Registry {
    pub fn insert(&self, headers: &HeaderMap, progress: Progress) {
        let Some(owner) = observer(headers) else {
            return;
        };
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, p| {
            p.0.lock()
                .unwrap_or_else(|e| e.into_inner())
                .started
                .elapsed()
                < TTL
        });
        if entries.len() >= LIMIT {
            if let Some(oldest) = entries
                .iter()
                .max_by_key(|(_, p)| {
                    p.0.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .started
                        .elapsed()
                })
                .map(|(k, _)| k.clone())
            {
                entries.remove(&oldest);
            }
        }
        let id = progress.snapshot().request_id;
        entries.insert((owner, id), progress);
    }
    fn get(&self, headers: &HeaderMap, id: &str) -> Option<Snapshot> {
        let owner = observer(headers)?;
        let entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let p = entries.get(&(owner, id.to_owned()))?;
        if p.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .started
            .elapsed()
            >= TTL
        {
            return None;
        }
        Some(p.snapshot())
    }
}
pub(super) async fn get(
    State(state): State<super::AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Snapshot>, ApiError> {
    state
        .request_progress
        .get(&headers, &id)
        .map(Json)
        .ok_or_else(|| {
            ApiError::not_found("Request diagnostics expired or are not available to this browser")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostics_respond_while_request_is_blocked_and_preserve_failure() {
        use axum::{Router, body::Body, http::Request, middleware, routing::get};
        use tower::ServiceExt;
        let state = super::super::app_state(super::super::ChronicleServerConfig::front_only());
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let app = Router::new()
            .route(
                "/api/slow",
                get({
                    let entered = entered.clone();
                    let release = release.clone();
                    move || {
                        let entered = entered.clone();
                        let release = release.clone();
                        async move {
                            phase("storage_read");
                            entered.notify_one();
                            release.notified().await;
                            Err::<(), _>(ApiError::invalid_request("source format is invalid"))
                        }
                    }
                }),
            )
            .route("/api/requests/{id}", get(super::get))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                super::super::request_log::warehouse_request_layer,
            ))
            .with_state(state.clone());
        let token = "0123456789abcdef0123456789abcdef";
        let request = |uri: &str, owner: &str| {
            Request::builder()
                .uri(uri)
                .header(OBSERVER_HEADER, owner)
                .header("x-request-id", "live-test")
                .body(Body::empty())
                .unwrap()
        };
        let task = tokio::spawn(
            app.clone()
                .oneshot(request("/api/slow?secret=not-recorded", token)),
        );
        entered.notified().await;
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            app.clone()
                .oneshot(request("/api/requests/live-test", token)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), 200);
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("not-recorded"));
        let snapshot: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(snapshot.state, "running");
        assert!(
            snapshot
                .phases
                .iter()
                .any(|p| p.name == "storage_read" && p.state == "running")
        );
        let denied = app
            .clone()
            .oneshot(request(
                "/api/requests/live-test",
                "ffffffffffffffffffffffffffffffff",
            ))
            .await
            .unwrap();
        assert_eq!(denied.status(), 404);
        release.notify_one();
        assert_eq!(task.await.unwrap().unwrap().status(), 400);
        let response = app
            .oneshot(request("/api/requests/live-test", token))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let snapshot: Snapshot = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(snapshot.state, "failed");
        assert_eq!(snapshot.error.as_deref(), Some("source format is invalid"));
        assert!(
            snapshot
                .phases
                .iter()
                .any(|p| p.name == "storage_read" && p.state == "failed")
        );
        assert_eq!(
            state.request_progress.0.lock().unwrap().len(),
            1,
            "polls are never registered"
        );
    }

    #[test]
    fn progress_is_live_isolated_bounded_and_finishes_on_cancel() {
        let registry = Registry::default();
        let mut headers = HeaderMap::new();
        headers.insert(
            OBSERVER_HEADER,
            "0123456789abcdef0123456789abcdef".parse().unwrap(),
        );
        let p = Progress::new(
            "id".into(),
            "GET".into(),
            "/api/explorer/runs".into(),
            false,
        );
        registry.insert(&headers, p.clone());
        p.phase("authentication");
        p.phase("worker_queue");
        assert!(registry.get(&HeaderMap::new(), "id").is_none());
        let snapshot = registry.get(&headers, "id").unwrap();
        assert_eq!(snapshot.phases[0].state, "completed");
        assert_eq!(snapshot.phases[1].state, "running");
        let mut other = headers.clone();
        other.insert(
            OBSERVER_HEADER,
            "ffffffffffffffffffffffffffffffff".parse().unwrap(),
        );
        assert!(registry.get(&other, "id").is_none());
        drop(CancelOnDrop(p));
        assert_eq!(registry.get(&headers, "id").unwrap().state, "cancelled");
        for i in 0..LIMIT + 5 {
            registry.insert(
                &headers,
                Progress::new(i.to_string(), "GET".into(), "/api/runs".into(), false),
            );
        }
        assert_eq!(registry.0.lock().unwrap().len(), LIMIT);
        let expired = Progress::new("expired".into(), "GET".into(), "/api/runs".into(), false);
        registry.insert(&headers, expired.clone());
        expired.0.lock().unwrap().started = Instant::now() - TTL;
        assert!(registry.get(&headers, "expired").is_none());
    }
}
