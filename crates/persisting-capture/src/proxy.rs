//! OpenAI-compatible HTTP proxy with capture on request/response.

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

use crate::admin::{admin_router, AdminState};
use crate::capture_call::CaptureCall;
use crate::config::{CaptureLevel, ProxyConfig};
use crate::conversion::{
    completions_response_to_messages, messages_request_to_completions, CompletionsStreamTranslator,
    ProtocolBridge,
};
use crate::debug::{self, is_debug_enabled, truncate_body_bytes};
use crate::dialogue_extract::{
    extract_assistant_text_from_json, extract_assistant_turn_from_sse,
    extract_user_message_from_request_body, push_sse_tool_snapshot, SseStreamBlockParser,
};
use crate::forward::{
    handle_connect, is_forward_proxy_request, is_llm_capture_path, transparent_forward,
};
use crate::models_list::build_models_response;
use crate::protocol::ProtocolKind;
use crate::provider::ProviderKind;
use crate::router::{resolve_route, rewrite_model_in_body};
use crate::run_config::load_session_proxy_config;
use crate::session_client::SessionClientRegistry;
use crate::session_index::{SessionIndexHandle, SessionIndexStore};
use crate::session_storage::{resolve_capture_route, route_config_key, CaptureRoute};
use crate::sink::{llm_request_summary_record, llm_response_record_with_content, CaptureSink};
use crate::subagent_link::{
    enrich_record, main_route_for_backfill, spawn_link_backfill_record, EnrichOutcome,
    SpawnLinkBackfill, SubagentRegistry,
};
use crate::usage::{
    estimate_cost_usd, extract_usage_from_response, extract_usage_from_sse, StreamMetrics,
};

#[derive(Clone)]
pub struct ProxyState {
    pub config: Arc<ProxyConfig>,
    pub storage: Arc<std::path::PathBuf>,
    pub client: reqwest::Client,
    pub sink: Arc<dyn CaptureSink>,
    pub index: SessionIndexHandle,
    pub session_clients: Arc<SessionClientRegistry>,
    pub subagent_registry: Arc<std::sync::Mutex<SubagentRegistry>>,
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
) -> anyhow::Result<()> {
    serve_with_shutdown(config, storage, sink, std::future::pending()).await
}

/// Run proxy until `shutdown` completes. Optionally signal bind readiness via `ready`.
pub async fn serve_with_shutdown(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureSink>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_with_shutdown_and_ready(config, storage, sink, None, shutdown).await
}

pub async fn serve_with_shutdown_and_ready(
    config: ProxyConfig,
    storage: impl AsRef<Path>,
    sink: Arc<dyn CaptureSink>,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop_tx.send(());
    });

    let storage = storage.as_ref().to_path_buf();
    let index_store = SessionIndexStore::open(&storage)?;
    let index = index_store.clone_handle();
    let started_at = chrono::Utc::now().to_rfc3339();

    if is_debug_enabled(&config, &storage) {
        tracing::debug!(
            target: "persisting_capture",
            "capture debug → {}",
            debug::debug_log_path(&storage).display()
        );
        debug::log_daemon_start(storage.as_path(), &config.listen, env!("CARGO_PKG_VERSION"));
    }

    let active_requests = Arc::new(AtomicUsize::new(0));
    let state = ProxyState {
        config: Arc::new(config.clone()),
        storage: Arc::new(storage),
        client: reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()?,
        sink,
        index: index.clone(),
        session_clients: Arc::new(SessionClientRegistry::default()),
        subagent_registry: Arc::new(std::sync::Mutex::new(SubagentRegistry::default())),
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
    let capture_level = cfg.capture_level;
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

    state.index.record_request(
        &agent_id,
        &session_id,
        provider,
        protocol.as_str(),
        &client_model,
    );
    let _ = state.index.flush();

    let mut upstream_body = body_bytes.clone();
    if resolved.model_rewritten {
        upstream_body = rewrite_model_in_body(&body_bytes, &upstream_model)?;
    }
    if bridge.needs_request_translation() {
        upstream_body = messages_request_to_completions(&upstream_body, &upstream_model)?;
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

    let (_, auth_source) = match crate::auth::resolve_upstream_api_key(route, &parts.headers) {
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
        match crate::auth::apply_upstream_headers(upstream_req, &parts.headers, route, protocol) {
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

    {
        let user_content = extract_user_message_from_request_body(&body_bytes);
        let body_json = serde_json::from_slice::<Value>(&body_bytes).ok();
        let forward_to = if resolved.model_rewritten {
            Some(upstream_model.as_str())
        } else {
            None
        };
        if let Err(e) = state.sink.append(&capture_route, &agent_id, {
            let mut rec = llm_request_summary_record(
                Some(capture_route.session_id.clone()),
                Some(agent_id.clone()),
                &client_model,
                &path,
                body_bytes.len(),
                protocol.as_str(),
                provider.as_str(),
                user_content,
                forward_to,
                &call,
                capture_level,
                body_json.as_ref(),
            );
            let outcome = if let Ok(mut reg) = state.subagent_registry.lock() {
                enrich_record(
                    &mut rec,
                    &capture_route,
                    &parts.headers,
                    body_json.as_ref(),
                    None,
                    &mut reg,
                )
            } else {
                EnrichOutcome::default()
            };
            apply_spawn_link_backfills(
                state.sink.as_ref(),
                &capture_route,
                &agent_id,
                &call,
                &outcome.spawn_link_backfills,
            );
            rec
        }) {
            tracing::warn!("request capture append: {e:#}");
        }
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
        return streaming_llm_response(
            upstream_resp,
            state,
            capture_route,
            agent_id,
            client_model,
            upstream_model,
            provider,
            protocol,
            bridge,
            debug_on,
            call,
            capture_level,
            parts.headers.clone(),
        )
        .await;
    }

    let mut resp_bytes = upstream_resp.bytes().await?;
    if bridge.needs_response_translation() {
        resp_bytes = completions_response_to_messages(&resp_bytes, &client_model)?;
    }
    finalize_llm_capture_response(
        state.sink.as_ref(),
        &state.index,
        state.storage.as_path(),
        &capture_route,
        &agent_id,
        &client_model,
        &upstream_model,
        provider,
        protocol,
        status.as_u16(),
        &resp_bytes,
        false,
        None,
        debug_on,
        &call,
        capture_level,
        &state.subagent_registry,
        &parts.headers,
        false,
        "",
    )
    .await?;

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
    route: CaptureRoute,
    agent_id: String,
    client_model: String,
    upstream_model: String,
    provider: ProviderKind,
    protocol: ProtocolKind,
    bridge: ProtocolBridge,
    debug_on: bool,
    call: CaptureCall,
    capture_level: CaptureLevel,
    request_headers: HeaderMap,
) -> anyhow::Result<Response> {
    let status = upstream_resp.status();
    let resp_headers = upstream_resp.headers().clone();
    let translate = bridge.needs_response_translation();
    let session_id = route.session_id.clone();

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

    let sink = Arc::clone(&state.sink);
    let index = state.index.clone();
    let storage = Arc::clone(&state.storage);
    let route_bg = route.clone();
    let agent_id_bg = agent_id.clone();
    let client_model_bg = client_model.clone();
    let upstream_model_bg = upstream_model.clone();
    let call_bg = call.clone();
    let registry = Arc::clone(&state.subagent_registry);
    let request_headers_bg = request_headers;

    tokio::spawn(async move {
        let mut buf = BytesMut::new();
        let mut sse_parser = SseStreamBlockParser::default();
        let mut stream_partial_emitted = false;
        let mut last_partial_content = String::new();
        let mut partial_index = 0u32;
        let mut translator = translate.then(|| CompletionsStreamTranslator::new(&client_model_bg));
        let mut stream = byte_stream;
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => {
                    buf.extend_from_slice(&chunk);
                    if let Ok(chunk_str) = std::str::from_utf8(&chunk) {
                        if let Some(partial) = push_sse_tool_snapshot(&mut sse_parser, chunk_str) {
                            append_stream_partial_assistant(
                                sink.as_ref(),
                                &route_bg,
                                &agent_id_bg,
                                &client_model_bg,
                                status.as_u16(),
                                &partial,
                                partial_index,
                                &call_bg,
                                capture_level,
                                &registry,
                                &request_headers_bg,
                            );
                            stream_partial_emitted = true;
                            last_partial_content = partial;
                            partial_index += 1;
                        }
                    }
                    let out = if let Some(t) = translator.as_mut() {
                        match t.push_chunk(&chunk) {
                            Ok(s) if !s.is_empty() => Bytes::from(s),
                            Ok(_) => continue,
                            Err(e) => {
                                tracing::warn!("stream translate: {e:#}");
                                chunk
                            }
                        }
                    } else {
                        chunk
                    };
                    if tx.send(Ok(out)).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = tx.send(Err(msg.clone()));
                    if debug_on {
                        debug::log_llm_upstream_error(
                            storage.as_path(),
                            &route_bg.session_id,
                            &agent_id_bg,
                            &client_model_bg,
                            "stream",
                            &msg,
                        );
                    }
                    break;
                }
            }
        }
        let stream_metrics = translator.as_ref().map(|t| t.metrics().clone());
        if let Some(t) = translator.as_mut() {
            if let Ok(tail) = t.finish_stream() {
                let _ = tx.send(Ok(Bytes::from(tail)));
            }
        }
        let resp_bytes = if translate {
            Bytes::from(
                translator
                    .map(|t| t.upstream_snapshot().to_string())
                    .unwrap_or_default(),
            )
        } else {
            buf.freeze()
        };
        if let Err(e) = finalize_llm_capture_response(
            sink.as_ref(),
            &index,
            storage.as_path(),
            &route_bg,
            &agent_id_bg,
            &client_model_bg,
            &upstream_model_bg,
            provider,
            protocol,
            status.as_u16(),
            &resp_bytes,
            true,
            stream_metrics.as_ref(),
            debug_on,
            &call_bg,
            capture_level,
            &registry,
            &request_headers_bg,
            stream_partial_emitted,
            &last_partial_content,
        )
        .await
        {
            tracing::warn!("stream capture finalize: {e:#}");
        }
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
    builder = attach_capture_headers(builder, &call);
    Ok(builder
        .body(Body::from_stream(body_stream))
        .map_err(|e| anyhow::anyhow!("response body: {e}"))?
        .into_response())
}

async fn finalize_llm_capture_response(
    sink: &dyn CaptureSink,
    index: &SessionIndexHandle,
    storage: &Path,
    route: &CaptureRoute,
    agent_id: &str,
    client_model: &str,
    upstream_model: &str,
    provider: ProviderKind,
    protocol: ProtocolKind,
    status: u16,
    resp_bytes: &Bytes,
    streaming: bool,
    stream_metrics: Option<&StreamMetrics>,
    debug_on: bool,
    call: &CaptureCall,
    level: CaptureLevel,
    subagent_registry: &Arc<std::sync::Mutex<SubagentRegistry>>,
    headers: &HeaderMap,
    stream_partial_emitted: bool,
    last_partial_content: &str,
) -> anyhow::Result<()> {
    let resp_text = std::str::from_utf8(resp_bytes).unwrap_or("<non-utf8>");
    let usage = if streaming {
        stream_metrics
            .map(|m| m.usage.clone())
            .filter(|u| u.total_tokens > 0 || u.input_tokens > 0 || u.output_tokens > 0)
            .unwrap_or_else(|| extract_usage_from_sse(resp_text))
    } else {
        let resp_json: Value = serde_json::from_slice(resp_bytes)
            .unwrap_or_else(|_| Value::String(resp_text.to_string()));
        extract_usage_from_response(&resp_json)
    };
    let resp_json = if streaming {
        Value::String(resp_text.to_string())
    } else {
        serde_json::from_slice(resp_bytes).unwrap_or_else(|_| Value::String(resp_text.to_string()))
    };

    let cost = estimate_cost_usd(upstream_model, provider, &usage);
    index.record_response(
        agent_id,
        &route.session_id,
        provider,
        client_model,
        &usage,
        cost,
    );
    let _ = index.flush();

    if debug_on {
        debug::log_llm_response(
            storage,
            &route.session_id,
            agent_id,
            client_model,
            status,
            usage.total_tokens,
            resp_text,
        );
    }

    let mut resp_payload = serde_json::json!({
        "status": status,
        "protocol": protocol.as_str(),
        "provider": provider.as_str(),
        "usage": usage,
        "estimated_cost_usd": cost,
    });
    if upstream_model != client_model {
        resp_payload["forward_to"] = serde_json::Value::String(upstream_model.to_string());
    }
    if level.includes_full_body() {
        resp_payload["body"] = resp_json.clone();
    }
    if let Some(m) = stream_metrics {
        if let Some(ttft) = m.ttft_ms {
            resp_payload["ttft_ms"] = serde_json::Value::Number(ttft.into());
        }
        if m.usage.reasoning_tokens > 0 {
            resp_payload["reasoning_tokens"] =
                serde_json::Value::Number(m.usage.reasoning_tokens.into());
        }
    }

    let assistant_content = if level.includes_assistant_text() {
        if streaming {
            Some(extract_assistant_turn_from_sse(resp_text))
        } else {
            extract_assistant_text_from_json(&resp_json)
        }
    } else {
        None
    };
    let assistant_content = trim_stream_partial_duplicate(
        assistant_content,
        stream_partial_emitted,
        last_partial_content,
    );

    let outcome = if let Ok(mut reg) = subagent_registry.lock() {
        let mut rec = llm_response_record_with_content(
            Some(route.session_id.clone()),
            Some(agent_id.to_string()),
            status,
            &resp_payload,
            streaming,
            assistant_content.clone(),
            call,
            level,
        );
        let outcome = enrich_record(
            &mut rec,
            route,
            headers,
            None,
            assistant_content.as_deref(),
            &mut reg,
        );
        sink.append(route, agent_id, rec)?;
        outcome
    } else {
        sink.append(
            route,
            agent_id,
            llm_response_record_with_content(
                Some(route.session_id.clone()),
                Some(agent_id.to_string()),
                status,
                &resp_payload,
                streaming,
                assistant_content,
                call,
                level,
            ),
        )?;
        EnrichOutcome::default()
    };
    apply_spawn_link_backfills(sink, route, agent_id, call, &outcome.spawn_link_backfills);

    Ok(())
}

fn trim_stream_partial_duplicate(
    assistant_content: Option<String>,
    stream_partial_emitted: bool,
    last_partial_content: &str,
) -> Option<String> {
    let Some(full) = assistant_content.filter(|s| !s.is_empty()) else {
        return None;
    };
    if !stream_partial_emitted || last_partial_content.is_empty() {
        return Some(full);
    }
    if full == last_partial_content {
        return None;
    }
    if let Some(tail) = full.strip_prefix(last_partial_content) {
        let tail = tail.trim_start();
        if tail.is_empty() {
            None
        } else {
            Some(tail.to_string())
        }
    } else {
        Some(full)
    }
}

fn apply_spawn_link_backfills(
    sink: &dyn CaptureSink,
    route: &CaptureRoute,
    agent_id: &str,
    call: &CaptureCall,
    backfills: &[SpawnLinkBackfill],
) {
    if backfills.is_empty() {
        return;
    }
    let main_route = main_route_for_backfill(route);
    for bf in backfills {
        let rec = spawn_link_backfill_record(&bf.parent_call_id, &bf.links, call);
        if let Err(e) = sink.append(&main_route, agent_id, rec) {
            tracing::warn!("spawn link backfill append: {e:#}");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn append_stream_partial_assistant(
    sink: &dyn CaptureSink,
    route: &CaptureRoute,
    agent_id: &str,
    client_model: &str,
    status: u16,
    partial_content: &str,
    partial_index: u32,
    call: &CaptureCall,
    level: CaptureLevel,
    subagent_registry: &Arc<std::sync::Mutex<SubagentRegistry>>,
    headers: &HeaderMap,
) {
    if partial_content.trim().is_empty() || !level.includes_assistant_text() {
        return;
    }
    let resp_payload = serde_json::json!({
        "status": status,
        "model": client_model,
        "stream_partial": true,
        "stream_partial_index": partial_index,
    });
    let outcome = if let Ok(mut reg) = subagent_registry.lock() {
        let mut rec = llm_response_record_with_content(
            Some(route.session_id.clone()),
            Some(agent_id.to_string()),
            status,
            &resp_payload,
            true,
            Some(partial_content.to_string()),
            call,
            level,
        );
        let outcome = enrich_record(
            &mut rec,
            route,
            headers,
            None,
            Some(partial_content),
            &mut reg,
        );
        if sink.append(route, agent_id, rec).is_err() {
            return;
        }
        outcome
    } else {
        return;
    };
    apply_spawn_link_backfills(sink, route, agent_id, call, &outcome.spawn_link_backfills);
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

fn effective_config(state: &ProxyState, route: &CaptureRoute) -> Arc<ProxyConfig> {
    load_session_proxy_config(state.storage.as_path(), route_config_key(route))
        .map(Arc::new)
        .unwrap_or_else(|| Arc::clone(&state.config))
}

fn extract_model(body: &Bytes) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("model")?.as_str().map(str::to_string)
}
