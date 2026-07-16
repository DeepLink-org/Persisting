use crate::capture::{CaptureEvent, CaptureMeta, CaptureSinkRouter, PostProcessorChain};
use crate::config::ProxyConfig;
use crate::dialogue::InferenceEndpoint;
use crate::router::RouteTable;
use crate::session::{
    RequestContext, RouteSessionMode, SessionResolveError, SessionSource, resolve_session,
};
use crate::tlv::new_call_id;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;

#[derive(Debug, Default)]
struct SessionTrack {
    requests: u64,
    next_seq: u64,
    next_turn: u64,
}

#[derive(Debug, Clone)]
struct InferenceCall {
    endpoint: InferenceEndpoint,
    route_mode: RouteSessionMode,
    path_session_id: Option<String>,
    request_path: String,
}

#[derive(Clone)]
pub struct AppState {
    client: reqwest::Client,
    config: Arc<ProxyConfig>,
    routes: Arc<RouteTable>,
    capture_sink: Arc<CaptureSinkRouter>,
    post_processors: Arc<PostProcessorChain>,
    sessions: Arc<RwLock<HashMap<String, SessionTrack>>>,
    errors: Arc<Mutex<VecDeque<String>>>,
}

impl AppState {
    pub fn new(config: ProxyConfig) -> Self {
        let client = reqwest::Client::new();
        let routes = RouteTable::from_config(&config);
        let tlv_root = PathBuf::from(config.store_dir.clone());
        let agent_id = config.agent_id.clone();
        let default_session_id = config.default_session_id.clone();
        let write_lock = Arc::new(Mutex::new(()));
        let tlv = crate::tlv::TlvWriter::new(
            tlv_root,
            agent_id,
            default_session_id,
            Arc::clone(&write_lock),
        );
        let config = Arc::new(config);
        let capture_sink = Arc::new(
            CaptureSinkRouter::new(Arc::clone(&config), tlv, write_lock)
                .expect("failed initializing capture sink router"),
        );
        Self {
            client,
            config,
            routes: Arc::new(routes),
            capture_sink,
            post_processors: Arc::new(PostProcessorChain::empty()),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            errors: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn listen_addr(&self) -> &str {
        &self.config.listen
    }

    pub fn admin_listen_addr(&self) -> &str {
        &self.config.admin_listen
    }

    async fn alloc_turn(&self, session_id: &str) -> (u64, u64, u64) {
        let mut sessions = self.sessions.write().await;
        let track = sessions
            .entry(session_id.to_string())
            .or_insert_with(|| SessionTrack {
                requests: 0,
                next_seq: 0,
                next_turn: 1,
            });
        track.requests += 1;
        let turn = track.next_turn;
        let user_seq = track.next_seq;
        let assistant_seq = track.next_seq + 1;
        track.next_seq += 2;
        track.next_turn += 1;
        (user_seq, assistant_seq, turn)
    }

    async fn push_error(&self, err: String) {
        let mut errors = self.errors.lock().await;
        if errors.len() >= 100 {
            errors.pop_front();
        }
        errors.push_back(err);
    }
}

pub fn build_public_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/models", get(v1_models))
        .route("/v1/chat/completions", post(flat_chat_completions))
        .route(
            "/v1/sessions/{session_id}/chat/completions",
            post(session_chat_completions),
        )
        .route(
            "/v1/sessions/{session_id}/responses",
            post(session_responses),
        )
        .with_state(state)
}

pub fn build_admin_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/admin/sessions", get(admin_sessions))
        .route("/admin/errors", get(admin_errors))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

async fn readyz() -> impl IntoResponse {
    Json(json!({"status": "ready"}))
}

async fn admin_sessions(State(state): State<AppState>) -> impl IntoResponse {
    let sessions = state.sessions.read().await;
    let mut rows = sessions
        .iter()
        .map(|(session_id, track)| {
            json!({
                "session_id": session_id,
                "requests": track.requests,
                "turns": track.next_turn.saturating_sub(1),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(
                b.get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    Json(json!({ "sessions": rows }))
}

async fn admin_errors(State(state): State<AppState>) -> impl IntoResponse {
    let errors = state.errors.lock().await;
    let rows = errors.iter().cloned().collect::<Vec<_>>();
    Json(json!({ "recent_errors": rows }))
}

async fn v1_models(State(state): State<AppState>) -> impl IntoResponse {
    let data = state
        .routes
        .list_models()
        .into_iter()
        .map(|model| {
            json!({
                "id": model.id,
                "object": "model",
                "owned_by": model.provider,
                "display_name": model.display_name,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "object": "list",
        "data": data
    }))
}

async fn flat_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    handle_inference(
        state,
        headers,
        payload,
        InferenceCall {
            endpoint: InferenceEndpoint::ChatCompletions,
            route_mode: RouteSessionMode::Flat,
            path_session_id: None,
            request_path: "/v1/chat/completions".to_string(),
        },
    )
    .await
}

async fn session_chat_completions(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let request_path = build_session_request_path(
        &state.config.base_session_path,
        &session_id,
        InferenceEndpoint::ChatCompletions,
    );
    handle_inference(
        state,
        headers,
        payload,
        InferenceCall {
            endpoint: InferenceEndpoint::ChatCompletions,
            route_mode: RouteSessionMode::SessionScoped,
            path_session_id: Some(session_id),
            request_path,
        },
    )
    .await
}

async fn session_responses(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let request_path = build_session_request_path(
        &state.config.base_session_path,
        &session_id,
        InferenceEndpoint::Responses,
    );
    handle_inference(
        state,
        headers,
        payload,
        InferenceCall {
            endpoint: InferenceEndpoint::Responses,
            route_mode: RouteSessionMode::SessionScoped,
            path_session_id: Some(session_id),
            request_path,
        },
    )
    .await
}

fn build_session_request_path(
    base_session_path: &str,
    session_id: &str,
    endpoint: InferenceEndpoint,
) -> String {
    format!(
        "{}/{}/{}",
        base_session_path.trim_end_matches('/'),
        session_id,
        endpoint.upstream_suffix()
    )
}

async fn handle_inference(
    state: AppState,
    headers: HeaderMap,
    payload: Value,
    call: InferenceCall,
) -> Response {
    let model = match payload.get("model").and_then(Value::as_str) {
        Some(model) => model,
        None => {
            return error_response(StatusCode::BAD_REQUEST, "missing field: model");
        }
    };

    let route = match state.routes.resolve_model(model).cloned() {
        Some(route) => route,
        None => {
            return error_response(StatusCode::BAD_REQUEST, "unknown model route");
        }
    };

    let ctx = RequestContext {
        mode: call.route_mode,
        path_session_id: call.path_session_id.as_deref(),
        headers: &headers,
        body: &payload,
    };
    let resolved = match resolve_session(&ctx, &state.config.session_settings()) {
        Ok(resolved) => resolved,
        Err(SessionResolveError::MissingPathSessionId) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "session_id must be supplied in URL path",
            );
        }
        Err(SessionResolveError::InvalidPathSessionId) => {
            return error_response(StatusCode::BAD_REQUEST, "invalid session_id in URL path");
        }
        Err(SessionResolveError::MissingSessionId | SessionResolveError::InvalidSessionId) => {
            return error_response(StatusCode::BAD_REQUEST, "unable to resolve session_id");
        }
    };

    if resolved.source == SessionSource::Default {
        tracing::warn!(
            session_id = %resolved.storage_session_id,
            "using default_session_id; inject session-scoped URL, header, or metadata.session_id"
        );
    }

    let session_id = resolved.storage_session_id;
    let upstream_url = format!(
        "{}/{}",
        route.upstream_base_url.trim_end_matches('/'),
        call.endpoint.upstream_suffix()
    );
    let mut request_builder = state.client.post(upstream_url).json(&payload);
    if let Some(api_key) = route.api_key.as_ref().filter(|key| !key.trim().is_empty()) {
        request_builder = request_builder.bearer_auth(api_key);
    }

    let upstream_response = match request_builder.send().await {
        Ok(response) => response,
        Err(err) => {
            state
                .push_error(format!("upstream request failed: {err}"))
                .await;
            return error_response(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
    };

    let status = StatusCode::from_u16(upstream_response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream_response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    let is_stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if is_stream {
        let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(16);
        let stream_state = state.clone();
        let stream_session_id = session_id;
        let stream_model = model.to_string();
        let stream_payload = payload;
        let stream_call = call.clone();
        let mut stream_upstream = upstream_response;
        let stream_status = status;
        tokio::spawn(async move {
            let mut raw = Vec::new();
            loop {
                match stream_upstream.chunk().await {
                    Ok(Some(chunk)) => {
                        raw.extend_from_slice(&chunk);
                        if tx.send(Ok(chunk)).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        stream_state
                            .push_error(format!("stream chunk read failed: {err}"))
                            .await;
                        let io_err =
                            std::io::Error::other(format!("stream chunk read failed: {err}"));
                        let _ = tx.send(Err(io_err)).await;
                        break;
                    }
                }
            }

            let raw_text = String::from_utf8_lossy(&raw).into_owned();
            let (response_text, usage, finish_reason) =
                summarize_stream_response(stream_call.endpoint, &raw_text);
            let (user_seq, assistant_seq, turn) = stream_state.alloc_turn(&stream_session_id).await;
            let call_id = new_call_id();
            persist_inference(
                &stream_state,
                &stream_call,
                stream_session_id,
                stream_model,
                stream_payload,
                HeaderMap::new(),
                stream_status.as_u16(),
                true,
                response_json_from_stream(stream_call.endpoint, &raw_text),
                response_text,
                usage,
                finish_reason,
                user_seq,
                assistant_seq,
                turn,
                call_id,
            )
            .await;
        });

        let relay_stream = ReceiverStream::new(rx);
        let mut builder = Response::builder().status(status);
        if let Some(content_type) = content_type.as_deref() {
            builder = builder.header(axum::http::header::CONTENT_TYPE, content_type);
        } else {
            builder = builder.header(axum::http::header::CONTENT_TYPE, "text/event-stream");
        }

        return match builder.body(Body::from_stream(relay_stream)) {
            Ok(response) => response,
            Err(err) => {
                state
                    .push_error(format!("building stream response failed: {err}"))
                    .await;
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to build stream response",
                )
            }
        };
    }

    let body_text = match upstream_response.text().await {
        Ok(text) => text,
        Err(err) => {
            state
                .push_error(format!("read upstream body failed: {err}"))
                .await;
            return error_response(StatusCode::BAD_GATEWAY, "failed reading upstream response");
        }
    };

    let response_json = serde_json::from_str::<Value>(&body_text).unwrap_or_else(|_| {
        json!({
            "raw_response": truncate_text(&body_text, 1200)
        })
    });
    let (response_text, usage, finish_reason) =
        summarize_json_response(call.endpoint, &response_json);
    let (user_seq, assistant_seq, turn) = state.alloc_turn(&session_id).await;
    let call_id = new_call_id();
    persist_inference(
        &state,
        &call,
        session_id,
        model.to_string(),
        payload,
        headers,
        status.as_u16(),
        false,
        response_json,
        response_text,
        usage,
        finish_reason,
        user_seq,
        assistant_seq,
        turn,
        call_id,
    )
    .await;

    let mut builder = Response::builder().status(status);
    if let Some(content_type) = content_type.as_deref() {
        builder = builder.header(axum::http::header::CONTENT_TYPE, content_type);
    } else {
        builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
    }

    match builder.body(Body::from(body_text)) {
        Ok(response) => response,
        Err(err) => {
            state
                .push_error(format!("building json response failed: {err}"))
                .await;
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to build upstream response",
            )
        }
    }
}

fn summarize_json_response(
    endpoint: InferenceEndpoint,
    value: &Value,
) -> (Option<String>, Option<Value>, Option<String>) {
    match endpoint {
        InferenceEndpoint::ChatCompletions => {
            let finish_reason = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str)
                .map(ToString::to_string);

            let response_text = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| {
                    choice
                        .get("message")
                        .and_then(|message| message.get("content"))
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .or_else(|| {
                            choice
                                .get("text")
                                .and_then(Value::as_str)
                                .map(ToString::to_string)
                        })
                })
                .or_else(|| {
                    value
                        .get("output_text")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                });
            (response_text, value.get("usage").cloned(), finish_reason)
        }
        InferenceEndpoint::Responses => {
            let (response_text, usage) = crate::dialogue::summarize_responses_json_response(value);
            (response_text, usage, None)
        }
    }
}

fn summarize_stream_response(
    endpoint: InferenceEndpoint,
    raw: &str,
) -> (Option<String>, Option<Value>, Option<String>) {
    match endpoint {
        InferenceEndpoint::ChatCompletions => {
            let (response_text, finish_reason, usage) = parse_chat_sse_response(raw);
            (response_text, usage, finish_reason)
        }
        InferenceEndpoint::Responses => {
            let (response_text, usage) = crate::dialogue::summarize_responses_sse_response(raw);
            (response_text, usage, None)
        }
    }
}

fn response_json_from_stream(endpoint: InferenceEndpoint, raw: &str) -> Value {
    match endpoint {
        InferenceEndpoint::ChatCompletions => {
            let (response_text, finish_reason, usage) = parse_chat_sse_response(raw);
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": response_text,
                    },
                    "finish_reason": finish_reason,
                }],
                "usage": usage,
            })
        }
        InferenceEndpoint::Responses => {
            json!({"output_text": summarize_stream_response(endpoint, raw).0})
        }
    }
}

fn headers_to_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (key, value) in headers.iter() {
        if let Ok(text) = value.to_str() {
            out.insert(key.as_str().to_string(), text.to_string());
        }
    }
    out
}

async fn persist_inference(
    state: &AppState,
    call: &InferenceCall,
    session_id: String,
    model: String,
    request: Value,
    headers: HeaderMap,
    status_code: u16,
    stream: bool,
    response_raw: Value,
    response_text: Option<String>,
    usage: Option<Value>,
    finish_reason: Option<String>,
    user_seq: u64,
    assistant_seq: u64,
    turn: u64,
    call_id: String,
) {
    let mut event = CaptureEvent {
        call_id,
        session_id,
        agent_id: state.config.agent_id.clone(),
        step_id: turn,
        turn,
        endpoint: call.endpoint,
        request_path: call.request_path.clone(),
        model,
        request,
        request_headers: headers_to_map(&headers),
        response_raw,
        response_text,
        stream,
        status_code,
        completed_at: Utc::now(),
        metadata: BTreeMap::new(),
        field_patches: BTreeMap::new(),
        capture_meta: CaptureMeta {
            finish_reason,
            usage,
            segment_kind: None,
        },
        user_seq,
        assistant_seq,
    };
    state.post_processors.apply(&mut event);
    if let Err(err) = state.capture_sink.dispatch(event).await {
        state
            .push_error(format!("capture sink dispatch failed: {err}"))
            .await;
    }
}

fn parse_chat_sse_response(raw: &str) -> (Option<String>, Option<String>, Option<Value>) {
    let mut content = String::new();
    let mut finish_reason = None;
    let mut usage = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("data:") {
            continue;
        }
        let data = trimmed.trim_start_matches("data:").trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let chunk = match serde_json::from_str::<Value>(data) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if let Some(delta_text) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("delta"))
            .and_then(|delta| delta.get("content"))
            .and_then(Value::as_str)
        {
            content.push_str(delta_text);
        }

        if content.is_empty() {
            if let Some(message_text) = chunk
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
            {
                content.push_str(message_text);
            }
        }

        if finish_reason.is_none() {
            finish_reason = chunk
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str)
                .map(ToString::to_string);
        }

        if usage.is_none() {
            usage = chunk.get("usage").cloned();
        }
    }

    let response_text = if content.is_empty() {
        None
    } else {
        Some(content)
    };
    (response_text, finish_reason, usage)
}

fn truncate_text(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_len).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}
