//! Proxy shared state and server lifecycle.

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use super::admin::{admin_router, AdminState};
use super::dispatch::build_router;
use super::reasoning::ReasoningCacheHandle;
use crate::config::ProxyConfig;
use crate::debug::{self, is_debug_enabled};
use crate::engine::CaptureEngine;
use crate::session_client::SessionClientRegistry;
use crate::session_index::{SessionIndexHandle, SessionIndexStore};
use crate::sink::CaptureSink;

#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<ProxyConfig>,
    pub storage: Arc<std::path::PathBuf>,
    pub client: reqwest::Client,
    pub sink: Arc<dyn CaptureSink>,
    pub capture_engine: CaptureEngine,
    pub index: SessionIndexHandle,
    pub session_clients: Arc<SessionClientRegistry>,
    pub reasoning_cache: Arc<ReasoningCacheHandle>,
    pub active_requests: Arc<AtomicUsize>,
    pub started_at: String,
}

pub async fn serve(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureSink>,
    stream_markdown: bool,
) -> anyhow::Result<()> {
    serve_with_shutdown(
        config,
        storage,
        sink,
        stream_markdown,
        std::future::pending(),
    )
    .await
}

/// Run proxy until `shutdown` completes. Optionally signal bind readiness via `ready`.
pub async fn serve_with_shutdown(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureSink>,
    stream_markdown: bool,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_with_shutdown_and_ready(config, storage, sink, stream_markdown, None, shutdown).await
}

pub async fn serve_with_shutdown_and_ready(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureSink>,
    stream_markdown: bool,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop_tx.send(());
    });

    let storage = Arc::new(storage.as_ref().to_path_buf());
    let index_store = SessionIndexStore::open(storage.as_path())?;
    let index = index_store.clone_handle();
    let started_at = chrono::Utc::now().to_rfc3339();

    if is_debug_enabled(&config, storage.as_path()) {
        tracing::debug!(
            target: "persisting_capture",
            "capture debug → {}",
            debug::debug_log_path(storage.as_path()).display()
        );
        debug::log_daemon_start(storage.as_path(), &config.listen, env!("CARGO_PKG_VERSION"));
    }

    let active_requests = Arc::new(AtomicUsize::new(0));
    let capture_engine = CaptureEngine::new(
        Arc::clone(&sink),
        index.clone(),
        Arc::clone(&storage),
        stream_markdown,
    )
    .await?;
    let capture_for_shutdown = capture_engine.clone();
    let state = ProxyState {
        config: Arc::new(config.clone()),
        storage,
        client: reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()?,
        sink,
        capture_engine,
        index: index.clone(),
        session_clients: Arc::new(SessionClientRegistry::default()),
        reasoning_cache: Arc::new(ReasoningCacheHandle::new()),
        active_requests: Arc::clone(&active_requests),
        started_at: started_at.clone(),
    };

    let admin_listen: std::net::SocketAddr = config.admin_listen.parse()?;
    let admin_state = AdminState {
        index,
        listen: config.listen.clone(),
        started_at,
        active_requests,
    };
    let admin_app = admin_router(admin_state);
    let admin_shutdown = wait_shutdown(stop_rx.clone());
    let admin_handle: JoinHandle<()> = tokio::spawn(async move {
        if let Ok(listener) = tokio::net::TcpListener::bind(admin_listen).await {
            tracing::debug!(target: "persisting_capture", "capture admin API on http://{admin_listen}");
            let _ = axum::serve(listener, admin_app)
                .with_graceful_shutdown(admin_shutdown)
                .await;
        }
    });

    let listen: std::net::SocketAddr = config.listen.parse()?;
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    if let Some(tx) = ready {
        let _ = tx.send(());
    }
    tracing::debug!(target: "persisting_capture", "capture LLM proxy on http://{listen}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(wait_shutdown(stop_rx))
    .await?;
    admin_handle.abort();
    capture_for_shutdown.shutdown().await?;
    Ok(())
}

async fn wait_shutdown(mut rx: tokio::sync::watch::Receiver<()>) {
    let _ = rx.changed().await;
}
