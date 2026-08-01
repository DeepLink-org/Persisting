//! Gateway state and composition with OverlayNet.

use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use persisting_control::{ControlController, PolicyControlController};
use tokio::task::JoinHandle;

use super::admin::{admin_router, AdminState};
use super::dispatch::build_router;
use super::reasoning::ReasoningCacheHandle;
use crate::config::ProxyConfig;
use crate::engine::CaptureEngine;
use crate::runtime::debug::{self, is_debug_enabled};
use crate::session::client::SessionClientRegistry;
use crate::session::index::SessionIndexStore;
use crate::sink::CaptureEventSink;

#[derive(Clone)]
pub(crate) struct GatewayState {
    pub(crate) config: Arc<ProxyConfig>,
    pub(crate) storage: Arc<std::path::PathBuf>,
    pub(crate) client: reqwest::Client,
    pub(crate) capture_engine: CaptureEngine,
    pub(crate) session_clients: Arc<SessionClientRegistry>,
    pub(crate) reasoning_cache: Arc<ReasoningCacheHandle>,
    pub(crate) control_controller: Arc<dyn ControlController>,
    pub(crate) active_requests: Arc<AtomicUsize>,
}

pub async fn serve(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
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
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_with_shutdown_and_ready(config, storage, sink, stream_markdown, None, shutdown).await
}

pub async fn serve_with_shutdown_and_ready(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_with_runtime_control(
        config,
        storage,
        sink,
        stream_markdown,
        Arc::new(PolicyControlController),
        ready,
        shutdown,
    )
    .await
}

/// Gateway sink with an injected runtime control state controller.
///
/// pVisor injects the controller; Gateway and OverlayNet apply model/network
/// transitions while retaining HTTP adaptation and trajectory extraction.
pub async fn serve_with_runtime_control(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    control_controller: Arc<dyn ControlController>,
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
            target: "persisting_gateway",
            "capture debug → {}",
            debug::debug_log_path(storage.as_path()).display()
        );
        debug::log_daemon_start(storage.as_path(), &config.listen, env!("CARGO_PKG_VERSION"));
    }

    let admin_listen: std::net::SocketAddr = config.admin_listen.parse().with_context(|| {
        format!(
            "invalid capture admin listen address `{}`",
            config.admin_listen
        )
    })?;
    let listen: std::net::SocketAddr = config
        .listen
        .parse()
        .with_context(|| format!("invalid overlaynet listen address `{}`", config.listen))?;
    let admin_listener = tokio::net::TcpListener::bind(admin_listen)
        .await
        .with_context(|| format!("bind capture admin API on {admin_listen}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind overlaynet gateway on {listen}"))?;

    let active_requests = Arc::new(AtomicUsize::new(0));
    let capture_engine = CaptureEngine::new(
        Arc::clone(&sink),
        index.clone(),
        Arc::clone(&storage),
        stream_markdown,
    )
    .await?;
    let capture_for_shutdown = capture_engine.clone();
    let state = GatewayState {
        config: Arc::new(config.clone()),
        storage,
        client: reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()?,
        capture_engine,
        session_clients: Arc::new(SessionClientRegistry::default()),
        reasoning_cache: Arc::new(ReasoningCacheHandle::new()),
        control_controller,
        active_requests: Arc::clone(&active_requests),
    };

    let admin_state = AdminState {
        index,
        listen: config.listen.clone(),
        admin_listen: config.admin_listen.clone(),
        started_at,
        active_requests,
    };
    let admin_app = admin_router(admin_state);
    let admin_shutdown = wait_shutdown(stop_rx.clone());
    let app = build_router(state);

    let admin_handle: JoinHandle<()> = tokio::spawn(async move {
        tracing::debug!(target: "persisting_gateway", "capture admin API on http://{admin_listen}");
        if let Err(error) = axum::serve(admin_listener, admin_app)
            .with_graceful_shutdown(admin_shutdown)
            .await
        {
            tracing::warn!(target: "persisting_gateway", %error, "capture admin API stopped");
        }
    });

    if let Some(tx) = ready {
        let _ = tx.send(());
    }
    tracing::debug!(target: "persisting_gateway", "capture LLM proxy on http://{listen}");
    let serve_result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(wait_shutdown(stop_rx))
    .await;
    admin_handle.abort();
    let shutdown_result = capture_for_shutdown.shutdown().await;
    serve_result.context("serve overlaynet gateway")?;
    shutdown_result
}

async fn wait_shutdown(mut rx: tokio::sync::watch::Receiver<()>) {
    let _ = rx.changed().await;
}
