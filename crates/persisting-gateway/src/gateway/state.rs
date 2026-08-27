//! Gateway state and composition with OverlayNet.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use anyhow::Context;
use persisting_agentctl::{ControlController, PolicyControlController};
use persisting_overlaynet::{BandwidthRegistry, InterceptionMetrics};
use tokio::task::JoinHandle;

use super::admin::{AdminState, admin_router};
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
    pub(crate) interception_metrics: InterceptionMetrics,
    pub(crate) bandwidth_registry: BandwidthRegistry,
    pub(crate) attempt_id: Option<String>,
}

pub(crate) struct GatewayRuntimeControl {
    pub(crate) controller: Arc<dyn ControlController>,
    pub(crate) interception_metrics: InterceptionMetrics,
    pub(crate) bandwidth_registry: BandwidthRegistry,
    pub(crate) attempt_id: Option<String>,
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

/// Run the Gateway with already-bound proxy and admin listeners.
///
/// Embedders can use this entry point to reserve every listener before
/// starting any service, report the resolved addresses for port `0`, and
/// coordinate one shutdown signal across multiple servers.
pub async fn serve_with_listeners_and_shutdown(
    mut config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    listener: tokio::net::TcpListener,
    admin_listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    config.listen = listener
        .local_addr()
        .context("read Gateway listen address")?
        .to_string();
    config.admin_listen = admin_listener
        .local_addr()
        .context("read Gateway admin listen address")?
        .to_string();
    config.validate()?;
    serve_with_bound_listeners(
        config,
        storage,
        sink,
        stream_markdown,
        GatewayRuntimeControl {
            controller: Arc::new(PolicyControlController),
            interception_metrics: InterceptionMetrics::default(),
            bandwidth_registry: BandwidthRegistry::default(),
            attempt_id: None,
        },
        listener,
        admin_listener,
        None,
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
    serve_with_runtime_control_and_metrics(
        config,
        storage,
        sink,
        stream_markdown,
        GatewayRuntimeControl {
            controller: control_controller,
            interception_metrics: InterceptionMetrics::default(),
            bandwidth_registry: BandwidthRegistry::default(),
            attempt_id: None,
        },
        ready,
        shutdown,
    )
    .await
}

pub(crate) async fn serve_with_runtime_control_and_metrics(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    runtime_control: GatewayRuntimeControl,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
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
    serve_with_bound_listeners(
        config,
        storage,
        sink,
        stream_markdown,
        runtime_control,
        listener,
        admin_listener,
        ready,
        shutdown,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn serve_with_bound_listeners(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureEventSink>,
    stream_markdown: bool,
    runtime_control: GatewayRuntimeControl,
    listener: tokio::net::TcpListener,
    admin_listener: tokio::net::TcpListener,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop_tx.send(());
    });

    let listen = listener
        .local_addr()
        .context("read Gateway listen address")?;
    let admin_listen = admin_listener
        .local_addr()
        .context("read Gateway admin listen address")?;
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

    let active_requests = Arc::new(AtomicUsize::new(0));
    let interception_metrics = runtime_control.interception_metrics;
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
            // Redirects must return to the proxy client so every destination
            // gets a fresh OverlayNet authorization decision. Following a
            // cross-origin Location here would bypass the policy gate.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()?,
        capture_engine,
        session_clients: Arc::new(SessionClientRegistry::default()),
        reasoning_cache: Arc::new(ReasoningCacheHandle::new()),
        control_controller: runtime_control.controller,
        active_requests: Arc::clone(&active_requests),
        interception_metrics: interception_metrics.clone(),
        bandwidth_registry: runtime_control.bandwidth_registry,
        attempt_id: runtime_control.attempt_id,
    };

    let admin_state = AdminState {
        index,
        listen: config.listen.clone(),
        admin_listen: config.admin_listen.clone(),
        started_at,
        active_requests,
        interception_metrics,
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
