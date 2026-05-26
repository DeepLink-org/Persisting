//! OpenAI-compatible HTTP proxy with capture on request/response.

pub mod admin;
pub mod auth;
pub mod cancel_stream;
pub mod capture_dispatch;
pub mod deepseek_reasoning;
pub mod forward;
pub mod http_headers;
pub mod models_list;
pub mod router;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::UnboundedReceiverStream;

use self::admin::{admin_router, AdminState};
use self::capture_dispatch::spawn_capture_apply;
use self::deepseek_reasoning::ReasoningCacheHandle;
use self::forward::{
    handle_connect, is_forward_proxy_request, is_llm_capture_path, transparent_forward,
};
use self::http_headers::is_websocket_upgrade;
use self::models_list::build_models_response;
use self::router::{resolve_route, rewrite_model_in_body};
use crate::capture_call::CaptureCall;
use crate::config::ProxyConfig;
use crate::conversion::{
    translate_request_for_bridge, translate_response_for_bridge, ProtocolBridge, StreamTranslator,
};
use crate::debug::{self, is_debug_enabled, truncate_body_bytes};
use crate::dialogue_extract::extract_user_message_from_request_body;
use crate::engine::{
    CaptureEngine, CaptureEvent, CaptureInvocation, LlmCallCancelled, LlmRequestCaptured,
    LlmResponseCompleted, LlmResponseDraftUpdated,
};
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::run_config::load_session_proxy_config;
use crate::session_client::SessionClientRegistry;
use crate::session_index::{SessionIndexHandle, SessionIndexStore};
use crate::session_storage::{resolve_capture_route, route_config_key, CaptureRoute};
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

pub fn router(state: ProxyState) -> Router {
    Router::new().fallback(any(proxy_handler)).with_state(state)
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
    let app = router(state);
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

async fn proxy_handler(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    State(state): State<ProxyState>,
    req: Request,
) -> Response {
    match dispatch(state, req, peer).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!("proxy error: {e:#}");
            (StatusCode::BAD_GATEWAY, format!("persisting-proxy: {e:#}")).into_response()
        }
    }
}

async fn dispatch(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
) -> anyhow::Result<Response> {
    state.active_requests.fetch_add(1, Ordering::Relaxed);
    let result = dispatch_impl(state.clone(), req, peer).await;
    state.active_requests.fetch_sub(1, Ordering::Relaxed);
    result
}

async fn dispatch_impl(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
) -> anyhow::Result<Response> {
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let log_route = resolve_capture_route(
        req.headers(),
        &Bytes::new(),
        &state.config.session_header,
        state.storage.as_path(),
    );
    let session_id = log_route.session_id.clone();
    let cfg = effective_config(&state, &log_route);
    let debug_on = is_debug_enabled(&cfg, state.storage.as_path());

    if *req.method() == Method::CONNECT {
        if debug_on {
            let target = req
                .uri()
                .authority()
                .map(|a| a.to_string())
                .unwrap_or_else(|| uri.clone());
            debug::log_connect(state.storage.as_path(), &target, &session_id);
        }
        return Ok(handle_connect(req).await);
    }

    let path = req.uri().path().to_string();
    if is_forward_proxy_request(req.method(), req.uri()) {
        if is_llm_capture_path(&path) {
            if debug_on {
                debug::log_dispatch(
                    state.storage.as_path(),
                    method.as_str(),
                    &uri,
                    &session_id,
                    "llm_capture",
                );
            }
            return llm_capture(state, req, peer, debug_on).await;
        }
        if debug_on {
            debug::log_dispatch(
                state.storage.as_path(),
                method.as_str(),
                &uri,
                &session_id,
                "forward",
            );
        }
        let resp = transparent_forward(&state.client, req).await?;
        if debug_on {
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp
                .into_body()
                .collect()
                .await
                .map_err(|e| anyhow::anyhow!("read forward body: {e}"))?
                .to_bytes();
            debug::log_forward(
                state.storage.as_path(),
                method.as_str(),
                &uri,
                &session_id,
                status.as_u16(),
                &String::from_utf8_lossy(&bytes),
            );
            let mut builder = Response::builder().status(status);
            for (name, value) in headers.iter() {
                builder = builder.header(name, value);
            }
            return Ok(builder
                .body(Body::from(bytes))
                .map_err(|e| anyhow::anyhow!("forward response: {e}"))?
                .into_response());
        }
        return Ok(resp.into_response());
    }

    if debug_on {
        debug::log_dispatch(
            state.storage.as_path(),
            method.as_str(),
            &uri,
            &session_id,
            "llm_gateway",
        );
    }
    llm_capture(state, req, peer, debug_on).await
}

async fn llm_capture(
    state: ProxyState,
    req: Request,
    peer: std::net::SocketAddr,
    debug_on: bool,
) -> anyhow::Result<Response> {
    let (parts, body) = req.into_parts();

    if is_websocket_upgrade(&parts.headers) {
        return Ok(Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .header("content-type", "text/plain; charset=utf-8")
            .body(Body::from(
                "WebSocket transport is not supported by persisting-proxy; use HTTPS",
            ))
            .map_err(|e| anyhow::anyhow!("websocket rejection response: {e}"))?
            .into_response());
    }

    let body_bytes = body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("read request body: {e}"))?
        .to_bytes();
    let path = parts.uri.path().to_string();
    let method = parts.method.clone();
    let protocol = ProtocolKind::from_path(&path);

    let capture_route = resolve_capture_route(
        &parts.headers,
        &body_bytes,
        &state.config.session_header,
        state.storage.as_path(),
    );
    let call = CaptureCall::from_headers(&parts.headers);
    let cfg = effective_config(&state, &capture_route);
    let agent_id = cfg.agent_id.clone();
    let session_id = capture_route.session_id.clone();

    if method == Method::GET && is_models_list_path(&path) {
        let json = build_models_response(&cfg);
        let bytes = serde_json::to_vec(&json).context("serialize /v1/models")?;
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(bytes))
            .context("build /v1/models response")?
            .into_response());
    }

    state
        .session_clients
        .ensure(state.storage.as_path(), &agent_id, &capture_route, peer);

    let client_model = extract_model(&body_bytes).unwrap_or_else(|| "_unknown".to_string());
    let resolved = resolve_route(&cfg.models, &client_model)?;
    let route = resolved.route;
    let upstream_model = resolved.upstream_model.clone();
    let bridge = ProtocolBridge::needed(protocol, route);
    let upstream_protocol = bridge.upstream_protocol(protocol);
    let provider = route.effective_provider(upstream_protocol);

    let capture_inv = capture_invocation(
        &state,
        &capture_route,
        &agent_id,
        &call,
        &parts.headers,
        &client_model,
        &upstream_model,
        provider,
        protocol,
        debug_on,
    );

    {
        let user_content = extract_user_message_from_request_body(&body_bytes);
        let body_json = serde_json::from_slice::<Value>(&body_bytes).ok();
        spawn_capture_apply(
            state.capture_engine.clone(),
            capture_inv.clone(),
            CaptureEvent::LlmRequest(LlmRequestCaptured {
                path: path.clone(),
                body_bytes: body_bytes.len(),
                user_content,
                body_json,
                model_rewritten: resolved.model_rewritten,
            }),
        );
    }

    let mut upstream_body = body_bytes.clone();
    if resolved.model_rewritten {
        upstream_body = rewrite_model_in_body(&body_bytes, &upstream_model)?;
    }
    if bridge.needs_request_translation() && !body_bytes.is_empty() {
        upstream_body = translate_request_for_bridge(
            bridge,
            &upstream_body,
            &upstream_model,
            Some(state.reasoning_cache.as_ref()),
        )?;
    }

    let upstream_path = bridge.upstream_path(&path);
    let mut upstream_url = route.resolve_upstream_url(&upstream_path, upstream_protocol)?;
    if let Some(q) = parts.uri.query() {
        upstream_url.set_query(Some(q));
    }

    if debug_on {
        let body_preview = truncate_body_bytes(&upstream_body);
        debug::log_llm_request(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            protocol.as_str(),
            &path,
            upstream_url.as_str(),
            &body_preview,
        );
    }

    let (_, auth_source) = match self::auth::resolve_upstream_api_key(route, &parts.headers) {
        Ok(v) => v,
        Err(e) => {
            if debug_on {
                debug::log_llm_upstream_error(
                    state.storage.as_path(),
                    &session_id,
                    &agent_id,
                    &client_model,
                    upstream_url.as_str(),
                    &format!("auth: {e:#}"),
                );
            }
            return Err(e);
        }
    };
    if debug_on {
        debug::log_llm_auth_resolved(state.storage.as_path(), &session_id, auth_source);
    }

    let mut upstream_req = state.client.request(method, upstream_url.clone());
    upstream_req = upstream_req.body(upstream_body.clone());
    upstream_req =
        match self::auth::apply_upstream_headers(upstream_req, &parts.headers, route, protocol) {
            Ok(r) => r,
            Err(e) => {
                if debug_on {
                    debug::log_llm_upstream_error(
                        state.storage.as_path(),
                        &session_id,
                        &agent_id,
                        &client_model,
                        upstream_url.as_str(),
                        &format!("auth: {e:#}"),
                    );
                }
                return Err(e);
            }
        };
    if debug_on {
        debug::log_llm_upstream_sending(
            state.storage.as_path(),
            &session_id,
            upstream_url.as_str(),
        );
    }

    let upstream_resp = match upstream_req.send().await {
        Ok(r) => r,
        Err(e) => {
            if debug_on {
                debug::log_llm_upstream_error(
                    state.storage.as_path(),
                    &session_id,
                    &agent_id,
                    &client_model,
                    upstream_url.as_str(),
                    &e.to_string(),
                );
            }
            return Err(anyhow::anyhow!("upstream request: {e}"));
        }
    };
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let stream_request = request_wants_stream(&upstream_body);

    if debug_on {
        let content_type = resp_headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-");
        debug::log_llm_upstream_headers(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            upstream_url.as_str(),
            status.as_u16(),
            content_type,
            stream_request,
        );
    }

    if should_stream_to_client(&resp_headers, &upstream_body) {
        return streaming_llm_response(upstream_resp, state, capture_inv, bridge).await;
    }

    let mut resp_bytes = upstream_resp.bytes().await?;
    if bridge.needs_response_translation() {
        resp_bytes = translate_response_for_bridge(bridge, &resp_bytes, &client_model)?;
    }
    spawn_capture_apply(
        state.capture_engine.clone(),
        capture_inv.clone(),
        CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
            status: status.as_u16(),
            resp_bytes: resp_bytes.clone(),
            streaming: false,
            stream_metrics: None,
            assistant_content: None,
        }),
    );

    let mut builder = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
        builder = builder.header(name, value);
    }
    builder = attach_capture_headers(builder, &call);
    Ok(builder
        .body(Body::from(resp_bytes))
        .map_err(|e| anyhow::anyhow!("response body: {e}"))?
        .into_response())
}

async fn streaming_llm_response(
    upstream_resp: reqwest::Response,
    state: ProxyState,
    inv: CaptureInvocation,
    bridge: ProtocolBridge,
) -> anyhow::Result<Response> {
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let translate = bridge.needs_response_translation();
    let session_id = inv.route.session_id.clone();
    let agent_id = inv.agent_id.clone();
    let client_model = inv.client_model.clone();
    let debug_on = inv.debug_on;

    if debug_on {
        debug::log_llm_stream_start(
            state.storage.as_path(),
            &session_id,
            &agent_id,
            &client_model,
            status.as_u16(),
        );
    }

    let byte_stream = upstream_resp.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<Bytes, String>>();

    let capture_engine = state.capture_engine.clone();
    let inv_bg = inv.clone();
    let storage = Arc::clone(&state.storage);
    let reasoning_cache = Arc::clone(&state.reasoning_cache);

    tokio::spawn(async move {
        let mut buf = BytesMut::new();
        let mut translator = StreamTranslator::new(bridge, &inv_bg.client_model);
        let mut last_draft_at = std::time::Instant::now();
        let mut last_draft_content = String::new();
        let mut client_disconnected = false;
        let mut stream = byte_stream;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    let out = if let Some(t) = translator.as_mut() {
                        match t.push_chunk(&chunk) {
                            Ok(s) if !s.is_empty() => Bytes::from(s),
                            Ok(_) => {
                                maybe_emit_stream_draft(
                                    &capture_engine,
                                    &inv_bg,
                                    status.as_u16(),
                                    t,
                                    &mut last_draft_at,
                                    &mut last_draft_content,
                                );
                                continue;
                            }
                            Err(e) => {
                                tracing::warn!("stream translate: {e:#}");
                                chunk
                            }
                        }
                    } else {
                        chunk
                    };
                    if let Some(t) = translator.as_ref() {
                        maybe_emit_stream_draft(
                            &capture_engine,
                            &inv_bg,
                            status.as_u16(),
                            t,
                            &mut last_draft_at,
                            &mut last_draft_content,
                        );
                    }
                    if tx.send(Ok(out)).is_err() {
                        client_disconnected = true;
                        break;
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = tx.send(Err(msg.clone()));
                    if debug_on {
                        debug::log_llm_upstream_error(
                            storage.as_path(),
                            &inv_bg.route.session_id,
                            &inv_bg.agent_id,
                            &inv_bg.client_model,
                            "stream",
                            &msg,
                        );
                    }
                    break;
                }
            }
        }

        if client_disconnected {
            spawn_capture_apply(
                capture_engine,
                inv_bg,
                CaptureEvent::LlmCallCancelled(LlmCallCancelled {
                    status: status.as_u16(),
                    bytes_received: buf.len(),
                    streaming: true,
                }),
            );
            return;
        }

        let stream_metrics = translator.as_ref().map(|t| t.metrics().clone());
        if let Some(t) = translator.as_mut() {
            if let Ok(tail) = t.finish_stream() {
                let _ = tx.send(Ok(Bytes::from(tail)));
            }
            let (tool_ids, reasoning) = t.drain_reasoning_snapshot();
            if !reasoning.is_empty() {
                reasoning_cache.remember(&tool_ids, &reasoning);
            }
        }
        let resp_bytes = if translate {
            Bytes::from(
                translator
                    .as_ref()
                    .map(|t| t.upstream_snapshot().to_string())
                    .unwrap_or_default(),
            )
        } else {
            buf.freeze()
        };
        let stream_assistant_text = translator
            .as_ref()
            .and_then(|t| t.streaming_capture_snapshot());
        spawn_capture_apply(
            capture_engine,
            inv_bg,
            CaptureEvent::LlmResponseCompleted(LlmResponseCompleted {
                status: status.as_u16(),
                resp_bytes,
                streaming: true,
                stream_metrics,
                assistant_content: stream_assistant_text,
            }),
        );
    });

    let body_stream =
        UnboundedReceiverStream::new(rx).map(|item| item.map_err(std::io::Error::other));

    let mut builder = Response::builder().status(status);
    for (name, value) in resp_headers.iter() {
        if translate && name.as_str().eq_ignore_ascii_case("content-type") {
            continue;
        }
        builder = builder.header(name, value);
    }
    if translate {
        builder = builder.header("content-type", "text/event-stream");
    }
    builder = attach_capture_headers(builder, &inv.call);
    Ok(builder
        .body(Body::from_stream(body_stream))
        .map_err(|e| anyhow::anyhow!("response body: {e}"))?
        .into_response())
}

const STREAM_DRAFT_MD_INTERVAL: Duration = Duration::from_millis(150);

fn maybe_emit_stream_draft(
    engine: &CaptureEngine,
    inv: &CaptureInvocation,
    status: u16,
    translator: &StreamTranslator,
    last_draft_at: &mut std::time::Instant,
    last_draft_content: &mut String,
) {
    let Some(snapshot) = translator.streaming_capture_snapshot() else {
        return;
    };
    if snapshot == *last_draft_content {
        return;
    }
    if !last_draft_content.is_empty() && last_draft_at.elapsed() < STREAM_DRAFT_MD_INTERVAL {
        return;
    }
    spawn_capture_apply(
        engine.clone(),
        inv.clone(),
        CaptureEvent::LlmResponseDraftUpdated(LlmResponseDraftUpdated {
            status,
            assistant_content: snapshot.clone(),
        }),
    );
    *last_draft_content = snapshot;
    *last_draft_at = std::time::Instant::now();
}

fn attach_capture_headers(
    builder: axum::http::response::Builder,
    call: &CaptureCall,
) -> axum::http::response::Builder {
    builder
        .header("x-persisting-call-id", call.call_id.as_str())
        .header("x-persisting-trace-id", call.trace_id.as_str())
}

fn is_models_list_path(path: &str) -> bool {
    let p = path.trim_end_matches('/');
    p.ends_with("/models") || p == "models" || p.ends_with("/v1/models")
}

fn request_wants_stream(body: &Bytes) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false)
}

fn should_stream_to_client(headers: &HeaderMap, request_body: &Bytes) -> bool {
    if request_wants_stream(request_body) {
        return true;
    }
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false)
}

fn effective_config(state: &ProxyState, route: &CaptureRoute) -> Arc<ProxyConfig> {
    load_session_proxy_config(state.storage.as_path(), route_config_key(route))
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(&state.config))
}

#[allow(clippy::too_many_arguments)]
fn capture_invocation(
    state: &ProxyState,
    route: &CaptureRoute,
    agent_id: &str,
    call: &CaptureCall,
    headers: &HeaderMap,
    client_model: &str,
    upstream_model: &str,
    provider: ProviderKind,
    protocol: ProtocolKind,
    debug_on: bool,
) -> CaptureInvocation {
    let cfg = effective_config(state, route);
    CaptureInvocation {
        route: route.clone(),
        agent_id: agent_id.to_string(),
        call: call.clone(),
        headers: headers.clone(),
        level: cfg.capture_level,
        client_model: client_model.to_string(),
        upstream_model: upstream_model.to_string(),
        provider,
        protocol,
        storage: Arc::clone(&state.storage),
        debug_on,
    }
}

fn extract_model(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn stream_request_enables_passthrough() {
        let body = Bytes::from_static(br#"{"model":"m","stream":true,"messages":[]}"#);
        assert!(should_stream_to_client(&HeaderMap::new(), &body));
    }

    #[test]
    fn sse_response_enables_passthrough() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "text/event-stream".parse().unwrap());
        let body = Bytes::from_static(b"{}");
        assert!(should_stream_to_client(&h, &body));
    }
}
