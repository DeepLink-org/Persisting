//! Admin HTTP API: status + session list.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::session_index::{SessionIndex, SessionIndexHandle};

#[derive(Clone)]
pub struct AdminState {
    pub index: SessionIndexHandle,
    pub listen: String,
    pub started_at: String,
    pub active_requests: Arc<std::sync::atomic::AtomicUsize>,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub listen: String,
    pub admin: String,
    pub started_at: String,
    pub active_requests: usize,
    pub sessions: Vec<crate::session_index::SessionSummary>,
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
        admin: String::new(),
        started_at: st.started_at.clone(),
        active_requests: st
            .active_requests
            .load(std::sync::atomic::Ordering::Relaxed),
        sessions: index.sessions,
    })
}

async fn sessions_handler(State(st): State<AdminState>) -> Json<SessionIndex> {
    Json(st.index.snapshot())
}
