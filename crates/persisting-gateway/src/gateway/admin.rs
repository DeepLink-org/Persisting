//! Gateway admin HTTP API: status + session list.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::session::index::{SessionIndex, SessionIndexHandle};
use persisting_overlaynet::{InterceptionMetrics, InterceptionProfile, InterceptionSnapshot};

#[derive(Clone)]
pub struct AdminState {
    pub index: SessionIndexHandle,
    pub listen: String,
    pub admin_listen: String,
    pub started_at: String,
    pub active_requests: Arc<std::sync::atomic::AtomicUsize>,
    pub interception_metrics: InterceptionMetrics,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub listen: String,
    pub admin: String,
    pub started_at: String,
    pub active_requests: usize,
    pub interception: InterceptionProfile,
    pub interception_metrics: InterceptionSnapshot,
    pub sessions: Vec<crate::session::index::SessionSummary>,
}

pub fn admin_router(state: AdminState) -> Router {
    Router::new()
        .route("/admin/status", get(status_handler))
        .route("/admin/sessions", get(sessions_handler))
        .with_state(state)
}

async fn status_handler(State(st): State<AdminState>) -> Json<StatusResponse> {
    let index = st.index.snapshot();
    Json(StatusResponse {
        listen: st.listen.clone(),
        admin: st.admin_listen.clone(),
        started_at: st.started_at.clone(),
        active_requests: st
            .active_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        interception: InterceptionProfile::explicit_proxy(),
        interception_metrics: st.interception_metrics.snapshot(),
        sessions: index.sessions,
    })
}

async fn sessions_handler(State(st): State<AdminState>) -> Json<SessionIndex> {
    Json(st.index.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::index::SessionIndexStore;

    #[tokio::test]
    async fn status_reports_both_listen_addresses() {
        let storage = tempfile::tempdir().unwrap();
        let index = SessionIndexStore::open(storage.path())
            .unwrap()
            .clone_handle();
        let response = status_handler(State(AdminState {
            index,
            listen: "127.0.0.1:9875".into(),
            admin_listen: "127.0.0.1:9876".into(),
            started_at: "2026-07-31T00:00:00Z".into(),
            active_requests: Arc::new(std::sync::atomic::AtomicUsize::new(3)),
            interception_metrics: InterceptionMetrics::default(),
        }))
        .await;

        assert_eq!(response.0.listen, "127.0.0.1:9875");
        assert_eq!(response.0.admin, "127.0.0.1:9876");
        assert_eq!(response.0.active_requests, 3);
        assert!(!response.0.interception.is_enforcing());
        assert_eq!(
            response.0.interception_metrics,
            InterceptionSnapshot::default()
        );
    }
}
