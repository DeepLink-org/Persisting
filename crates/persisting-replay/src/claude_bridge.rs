use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Map, Value};
use tokio::sync::{oneshot, Notify};

use crate::claude_resume::{clean_resume_transport_envelope, ResumeTransportManifest};
use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

const BRIDGE_VERSION: &str = "sandbox-replay-anthropic-openai-bridge/1";
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 8192;
const DEFAULT_MODEL_CONTEXT_TOKENS: usize = 200_000;
const DEFAULT_CONTEXT_SAFETY_TOKENS: usize = 1024;
const DEFAULT_UPSTREAM_TIMEOUT_SECONDS: f64 = 300.0;
const DEFAULT_RETRY_DELAYS_SECONDS: &[f64] = &[15.0, 30.0, 60.0];
const RETRYABLE_UPSTREAM_STATUSES: &[u16] = &[408, 429, 500, 502, 503, 504];
const BRIDGE_START_TIMEOUT: Duration = Duration::from_secs(10);
const BRIDGE_STOP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ClaudeBridgeHandle {
    pub base_url: String,
    pub api_key: String,
    shared: Arc<BridgeShared>,
    shutdown: Option<oneshot::Sender<()>>,
    worker_done: Option<mpsc::Receiver<anyhow::Result<()>>>,
    worker: Option<JoinHandle<()>>,
}

struct BridgeShared {
    resume: Mutex<ResumeState>,
    client: reqwest::Client,
    upstream_url: String,
    upstream_api_key: String,
    model_name: String,
    routing_session_id: String,
    bridge_api_key: String,
    disable_thinking: bool,
    boundary_user_prompt: Option<String>,
    max_output_tokens: usize,
    model_context_tokens: usize,
    context_safety_tokens: usize,
    upstream_timeout: Duration,
    retry_delays: Vec<Duration>,
    cancelled: AtomicBool,
    cancel_notify: Notify,
}

struct ResumeState {
    manifest: ResumeTransportManifest,
    request_sequence: usize,
    forwarded_requests: usize,
    pending_forward_sequence: Option<usize>,
    failed: bool,
    failure: Option<String>,
}

impl ResumeState {
    fn clean(&mut self, payload: &Value) -> anyhow::Result<(Value, usize)> {
        if self.failed {
            anyhow::bail!(
                "Resume transport state is FAILED after an earlier rejected request: {}",
                self.failure
                    .as_deref()
                    .unwrap_or("unknown protocol failure")
            );
        }
        if self.pending_forward_sequence.is_some() {
            self.fail("another request arrived before the previous request was forwarded");
            anyhow::bail!(
                "Resume transport received another request before the previous validated request was forwarded"
            );
        }
        self.request_sequence += 1;
        let sequence = self.request_sequence;
        match clean_resume_transport_envelope(payload, &self.manifest, sequence) {
            Ok(cleaned) => {
                self.pending_forward_sequence = Some(sequence);
                Ok((cleaned.payload, sequence))
            }
            Err(error) => {
                self.fail(error.to_string());
                Err(error)
            }
        }
    }

    fn mark_forwarded(&mut self, sequence: usize) -> anyhow::Result<()> {
        if self.failed || self.pending_forward_sequence != Some(sequence) {
            self.fail("forwarded sequence did not match the pending validated request");
            anyhow::bail!(
                "Resume transport forwarded sequence does not match the pending validated request"
            );
        }
        self.pending_forward_sequence = None;
        self.forwarded_requests += 1;
        Ok(())
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.failed = true;
        if self.failure.is_none() {
            self.failure = Some(message.into());
        }
    }
}

impl ClaudeBridgeHandle {
    pub fn start(
        manifest: ResumeTransportManifest,
        routing_session_id: &str,
        disable_thinking: bool,
        boundary_user_prompt: Option<&str>,
    ) -> Result<Self, ReplayError> {
        let upstream_base = first_nonempty_env(&[
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "LLM_BASE_URL",
        ])
        .map(|(_, value)| value)
        .ok_or_else(|| {
            ReplayError::configuration(
                "Claude SandboxReplay bridge requires OPENAI_BASE_URL, OPENAI_API_BASE, or LLM_BASE_URL",
            )
        })?;
        let upstream_api_key = first_nonempty_env(&["OPENAI_API_KEY", "LLM_API_KEY"])
            .map(|(_, value)| value)
            .ok_or_else(|| {
                ReplayError::configuration(
                    "Claude SandboxReplay bridge requires OPENAI_API_KEY or LLM_API_KEY",
                )
            })?;
        let model_name = first_nonempty_env(&["MODEL_NAME", "LLM_MODEL"])
            .map(|(_, value)| value)
            .ok_or_else(|| {
                ReplayError::configuration(
                    "Claude SandboxReplay bridge requires MODEL_NAME or LLM_MODEL",
                )
            })?;
        let upstream_url = chat_completions_url(&upstream_base)?;
        let bridge_api_key = format!("pvisor-sandbox-replay-{}", uuid::Uuid::new_v4().simple());
        let listener = TcpListener::bind(("127.0.0.1", 0)).replay_context(
            ReplayErrorKind::Continuation,
            "allocate Claude SandboxReplay bridge port",
        )?;
        let address = listener.local_addr().replay_context(
            ReplayErrorKind::Continuation,
            "read Claude SandboxReplay bridge address",
        )?;
        listener.set_nonblocking(true).replay_context(
            ReplayErrorKind::Continuation,
            "configure Claude SandboxReplay bridge listener",
        )?;

        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .replay_context(
                ReplayErrorKind::Continuation,
                "build Claude SandboxReplay bridge client",
            )?;
        let shared = Arc::new(BridgeShared {
            resume: Mutex::new(ResumeState {
                manifest,
                request_sequence: 0,
                forwarded_requests: 0,
                pending_forward_sequence: None,
                failed: false,
                failure: None,
            }),
            client,
            upstream_url,
            upstream_api_key,
            model_name,
            routing_session_id: routing_session_id.to_owned(),
            bridge_api_key: bridge_api_key.clone(),
            disable_thinking,
            boundary_user_prompt: boundary_user_prompt.map(str::to_owned),
            max_output_tokens: integer_environment(
                "SANDBOX_PLAYBACK_BRIDGE_MAX_OUTPUT_TOKENS",
                DEFAULT_MAX_OUTPUT_TOKENS,
                1,
            )?,
            model_context_tokens: integer_environment(
                "SANDBOX_PLAYBACK_BRIDGE_MODEL_CONTEXT_TOKENS",
                DEFAULT_MODEL_CONTEXT_TOKENS,
                1,
            )?,
            context_safety_tokens: integer_environment(
                "SANDBOX_PLAYBACK_BRIDGE_CONTEXT_SAFETY_TOKENS",
                DEFAULT_CONTEXT_SAFETY_TOKENS,
                0,
            )?,
            upstream_timeout: Duration::from_secs_f64(float_environment(
                "SANDBOX_PLAYBACK_BRIDGE_UPSTREAM_TIMEOUT_SECONDS",
                DEFAULT_UPSTREAM_TIMEOUT_SECONDS,
                0.001,
            )?),
            retry_delays: retry_delays_environment()?,
            cancelled: AtomicBool::new(false),
            cancel_notify: Notify::new(),
        });
        let router = router(Arc::clone(&shared));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("pvisor-claude-replay-bridge".into())
            .spawn(move || {
                let result = run_bridge_worker(listener, router, shutdown_rx, ready_tx);
                let _ = done_tx.send(result);
            })
            .replay_context(
                ReplayErrorKind::Continuation,
                "start Claude SandboxReplay bridge thread",
            )?;
        let mut handle = Self {
            base_url: format!("http://{address}"),
            api_key: bridge_api_key,
            shared,
            shutdown: Some(shutdown_tx),
            worker_done: Some(done_rx),
            worker: Some(worker),
        };
        let startup_error = match ready_rx.recv_timeout(BRIDGE_START_TIMEOUT) {
            Ok(Ok(())) => None,
            Ok(Err(message)) => Some(message),
            Err(mpsc::RecvTimeoutError::Timeout) => Some(format!(
                "Claude SandboxReplay bridge did not become ready within {} seconds",
                BRIDGE_START_TIMEOUT.as_secs()
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some("Claude SandboxReplay bridge worker exited before reporting readiness".into())
            }
        };
        if let Some(mut message) = startup_error {
            if let Err(error) = handle.stop_worker() {
                message.push_str(&format!("; cleanup also failed: {error}"));
            }
            return Err(ReplayError::continuation(message));
        }
        Ok(handle)
    }

    pub fn child_environment(&self) -> BTreeMap<String, String> {
        let no_proxy = merged_no_proxy_environment();
        let mut environment = BTreeMap::from([
            ("ANTHROPIC_BASE_URL".into(), self.base_url.clone()),
            ("ANTHROPIC_API_KEY".into(), self.api_key.clone()),
            ("ANTHROPIC_AUTH_TOKEN".into(), self.api_key.clone()),
            ("ANTHROPIC_MODEL".into(), self.shared.model_name.clone()),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            ),
            ("IS_SANDBOX".into(), "1".into()),
        ]);
        environment.insert("NO_PROXY".into(), no_proxy.clone());
        environment.insert("no_proxy".into(), no_proxy);
        environment
    }

    pub fn finish(mut self) -> Result<usize, ReplayError> {
        self.stop_worker()?;
        let state = self
            .shared
            .resume
            .lock()
            .map_err(|_| ReplayError::continuation("Claude bridge state lock poisoned"))?;
        if state.failed {
            return Err(ReplayError::continuation(format!(
                "Claude Resume Transport bridge failed closed: {}",
                state
                    .failure
                    .as_deref()
                    .unwrap_or("unknown protocol failure")
            )));
        }
        if let Some(sequence) = state.pending_forward_sequence {
            return Err(ReplayError::continuation(format!(
                "Claude Resume Transport request {sequence} was validated but not forwarded"
            )));
        }
        if state.forwarded_requests == 0 {
            return Err(ReplayError::continuation(
                "Claude continuation made no validated model request through the SandboxReplay bridge",
            ));
        }
        Ok(state.forwarded_requests)
    }

    fn stop_worker(&mut self) -> Result<(), ReplayError> {
        self.shared.cancelled.store(true, Ordering::Release);
        self.shared.cancel_notify.notify_waiters();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }

        let worker_result = match self.worker_done.take() {
            Some(done) => match done.recv_timeout(BRIDGE_STOP_TIMEOUT) {
                Ok(result) => Some(result),
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.worker.take();
                    return Err(ReplayError::continuation(format!(
                        "Claude SandboxReplay bridge did not stop within {} seconds; detached worker",
                        BRIDGE_STOP_TIMEOUT.as_secs()
                    )));
                }
            },
            None => None,
        };

        if let Some(worker) = self.worker.take() {
            if worker.join().is_err() {
                return Err(ReplayError::continuation(
                    "Claude SandboxReplay bridge thread panicked",
                ));
            }
        }
        if let Some(result) = worker_result {
            result.replay_context(
                ReplayErrorKind::Continuation,
                "stop Claude SandboxReplay bridge",
            )?;
        }
        Ok(())
    }
}

fn run_bridge_worker(
    listener: TcpListener,
    router: Router,
    shutdown_rx: oneshot::Receiver<()>,
    ready_tx: mpsc::SyncSender<std::result::Result<(), String>>,
) -> anyhow::Result<()> {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("build bridge runtime: {error}")));
            return Err(error.into());
        }
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = ready_tx.send(Err(format!("adopt bridge listener: {error}")));
                return Err(error.into());
            }
        };
        ready_tx
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("bridge startup receiver was dropped"))?;
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await?;
        anyhow::Ok(())
    })
}

impl Drop for ClaudeBridgeHandle {
    fn drop(&mut self) {
        let _ = self.stop_worker();
    }
}

fn router(shared: Arc<BridgeShared>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/messages", post(messages))
        .route("/v1/messages", post(messages))
        .route("/messages/count_tokens", post(count_tokens))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(shared)
}

async fn health(State(shared): State<Arc<BridgeShared>>) -> Response {
    let (failed, request_sequence) = shared
        .resume
        .lock()
        .map(|state| (state.failed, state.request_sequence))
        .unwrap_or((true, 0));
    json_response(
        StatusCode::OK,
        json!({
            "status": "healthy",
            "bridge_version": BRIDGE_VERSION,
            "resume_mode": true,
            "resume_failed": failed,
            "request_sequence": request_sequence,
        }),
    )
}

async fn count_tokens(
    State(shared): State<Arc<BridgeShared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&shared, &headers) {
        return authentication_error();
    }
    let payload = match json_object(&body) {
        Ok(payload) => payload,
        Err(error) => return invalid_request(StatusCode::BAD_REQUEST, error),
    };
    json_response(
        StatusCode::OK,
        json!({"input_tokens": estimate_input_tokens(&payload)}),
    )
}

async fn messages(
    State(shared): State<Arc<BridgeShared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&shared, &headers) {
        return authentication_error();
    }
    let payload = match json_object(&body) {
        Ok(payload) => payload,
        Err(error) => return invalid_request(StatusCode::BAD_REQUEST, error),
    };
    let (mut cleaned, sequence) = {
        let mut resume = match shared.resume.lock() {
            Ok(resume) => resume,
            Err(_) => {
                return invalid_request(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Resume transport state lock poisoned".into(),
                )
            }
        };
        match resume.clean(&payload) {
            Ok(cleaned) => cleaned,
            Err(error) => {
                return invalid_request(StatusCode::UNPROCESSABLE_ENTITY, error.to_string())
            }
        }
    };
    if let Err(error) = inject_boundary_user_prompt(
        &mut cleaned,
        shared.boundary_user_prompt.as_deref(),
        sequence,
    ) {
        fail_resume(
            &shared,
            format!("boundary user prompt injection failed: {error}"),
        );
        return invalid_request(StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
    }
    let request = match openai_request(
        &cleaned,
        &shared.model_name,
        shared.max_output_tokens,
        shared.model_context_tokens,
        shared.context_safety_tokens,
        shared.disable_thinking,
    ) {
        Ok(request) => request,
        Err(error) => {
            fail_resume(&shared, format!("request conversion failed: {error}"));
            return invalid_request(StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
        }
    };
    {
        let mut resume = match shared.resume.lock() {
            Ok(resume) => resume,
            Err(_) => {
                return invalid_request(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "Resume transport state lock poisoned".into(),
                )
            }
        };
        if let Err(error) = resume.mark_forwarded(sequence) {
            return invalid_request(StatusCode::UNPROCESSABLE_ENTITY, error.to_string());
        }
    }

    let serialized = match serde_json::to_vec(&request) {
        Ok(serialized) => serialized,
        Err(error) => {
            return upstream_error(
                StatusCode::BAD_GATEWAY,
                format!("serialize upstream request: {error}"),
            )
        }
    };
    let (status, upstream_body) = match forward_openai_request(&shared, serialized).await {
        Ok(result) => result,
        Err(error) => return upstream_error(StatusCode::BAD_GATEWAY, error.to_string()),
    };
    if status != StatusCode::OK.as_u16() {
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        return upstream_error(
            status,
            String::from_utf8_lossy(&upstream_body[..upstream_body.len().min(1000)]).into_owned(),
        );
    }
    let upstream: Value = match serde_json::from_slice(&upstream_body) {
        Ok(Value::Object(value)) => Value::Object(value),
        Ok(_) => {
            return upstream_error(
                StatusCode::BAD_GATEWAY,
                "Upstream response must be a JSON object".into(),
            )
        }
        Err(error) => {
            return upstream_error(
                StatusCode::BAD_GATEWAY,
                format!("parse upstream response: {error}"),
            )
        }
    };
    let message = match anthropic_response(&cleaned, &upstream, &shared.model_name) {
        Ok(message) => message,
        Err(error) => return upstream_error(StatusCode::BAD_GATEWAY, error.to_string()),
    };
    if cleaned.get("stream").and_then(Value::as_bool) == Some(true) {
        sse_response(&message)
    } else {
        json_response(StatusCode::OK, message)
    }
}

fn inject_boundary_user_prompt(
    payload: &mut Value,
    prompt: Option<&str>,
    request_sequence: usize,
) -> anyhow::Result<bool> {
    let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) else {
        return Ok(false);
    };
    if request_sequence != 1 {
        return Ok(false);
    }
    let messages = payload
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("cleaned payload.messages must be an array"))?;
    messages.push(json!({"role": "user", "content": prompt}));
    Ok(true)
}

async fn not_found() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        json!({"error": {"message": "not found"}}),
    )
}

fn authorized(shared: &BridgeShared, headers: &HeaderMap) -> bool {
    let supplied = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        });
    supplied == Some(shared.bridge_api_key.as_str())
}

fn authentication_error() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid API key"},
        }),
    )
}

fn invalid_request(status: StatusCode, message: String) -> Response {
    json_response(
        status,
        json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": message,
            },
        }),
    )
}

fn upstream_error(status: StatusCode, message: String) -> Response {
    json_response(
        status,
        json!({
            "type": "error",
            "error": {"type": "api_error", "message": message},
        }),
    )
}

fn json_response(status: StatusCode, payload: Value) -> Response {
    let mut response = Response::new(Body::from(payload.to_string()));
    *response.status_mut() = status;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

fn sse_response(message: &Value) -> Response {
    let mut rendered = String::new();
    for (event, payload) in sse_events(message) {
        rendered.push_str("event: ");
        rendered.push_str(event);
        rendered.push_str("\ndata: ");
        rendered.push_str(&payload.to_string());
        rendered.push_str("\n\n");
    }
    let mut response = Response::new(Body::from(rendered));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn json_object(body: &[u8]) -> Result<Value, String> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(value)) => Ok(Value::Object(value)),
        Ok(_) => Err("payload must be a JSON object".into()),
        Err(error) => Err(format!("invalid JSON: {error}")),
    }
}

fn fail_resume(shared: &BridgeShared, message: String) {
    if let Ok(mut resume) = shared.resume.lock() {
        resume.fail(message);
    }
}

async fn forward_openai_request(
    shared: &BridgeShared,
    body: Vec<u8>,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let attempts = shared.retry_delays.len() + 1;
    for attempt in 0..attempts {
        if shared.cancelled.load(Ordering::Acquire) {
            anyhow::bail!("Claude SandboxReplay bridge request was cancelled during shutdown");
        }
        let send = shared
            .client
            .post(&shared.upstream_url)
            .timeout(shared.upstream_timeout)
            .header(
                AUTHORIZATION.as_str(),
                format!("Bearer {}", shared.upstream_api_key),
            )
            .header(CONTENT_TYPE.as_str(), "application/json")
            .header("X-LiteLLM-Session-ID", &shared.routing_session_id)
            .body(body.clone())
            .send();
        let result = tokio::select! {
            biased;
            _ = wait_for_cancellation(shared) => {
                anyhow::bail!("Claude SandboxReplay bridge request was cancelled during shutdown");
            }
            result = send => result,
        };
        match result {
            Ok(response) => {
                let status = response.status().as_u16();
                let body_result = tokio::select! {
                    biased;
                    _ = wait_for_cancellation(shared) => {
                        anyhow::bail!("Claude SandboxReplay bridge request was cancelled during shutdown");
                    }
                    result = response.bytes() => result,
                };
                match body_result {
                    Ok(response_body) => {
                        let response_body = response_body.to_vec();
                        if !RETRYABLE_UPSTREAM_STATUSES.contains(&status) || attempt + 1 == attempts
                        {
                            return Ok((status, response_body));
                        }
                    }
                    Err(error) if attempt + 1 == attempts => return Err(error.into()),
                    Err(_) => {}
                }
            }
            Err(error) if attempt + 1 == attempts => return Err(error.into()),
            Err(_) => {}
        }
        tokio::select! {
            biased;
            _ = wait_for_cancellation(shared) => {
                anyhow::bail!("Claude SandboxReplay bridge request was cancelled during shutdown");
            }
            _ = tokio::time::sleep(shared.retry_delays[attempt]) => {}
        }
    }
    unreachable!("upstream retry loop always returns")
}

async fn wait_for_cancellation(shared: &BridgeShared) {
    loop {
        let notified = shared.cancel_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if shared.cancelled.load(Ordering::Acquire) {
            return;
        }
        notified.await;
        if shared.cancelled.load(Ordering::Acquire) {
            return;
        }
    }
}

fn openai_request(
    payload: &Value,
    model_name: &str,
    max_output_tokens: usize,
    model_context_tokens: usize,
    context_safety_tokens: usize,
    disable_thinking: bool,
) -> anyhow::Result<Value> {
    let input_tokens = estimate_input_tokens(payload);
    let available_output_tokens = model_context_tokens
        .checked_sub(context_safety_tokens)
        .and_then(|remaining| remaining.checked_sub(input_tokens))
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "input token count exceeds the maximum number of tokens: estimated_input={input_tokens}, context={model_context_tokens}, reserved={context_safety_tokens}"
            )
        })?;
    let requested_output_tokens =
        parse_requested_output_tokens(payload.get("max_tokens"), max_output_tokens)?;
    let temperature = parse_temperature(payload.get("temperature"))?;
    let mut request = Map::new();
    request.insert("model".into(), Value::String(model_name.to_owned()));
    request.insert("messages".into(), Value::Array(openai_messages(payload)?));
    request.insert(
        "max_tokens".into(),
        json!(requested_output_tokens
            .min(max_output_tokens)
            .min(available_output_tokens)),
    );
    request.insert("temperature".into(), json!(temperature));
    let tools = openai_tools(payload)?;
    if !tools.is_empty() {
        request.insert("tools".into(), Value::Array(tools));
        request.insert(
            "tool_choice".into(),
            tool_choice(payload.get("tool_choice"))?.unwrap_or_else(|| json!("auto")),
        );
    }
    if disable_thinking {
        request.insert(
            "chat_template_kwargs".into(),
            json!({"enable_thinking": false}),
        );
        request.insert("reasoning_effort".into(), json!("none"));
    }
    Ok(Value::Object(request))
}

fn parse_requested_output_tokens(value: Option<&Value>, default: usize) -> anyhow::Result<usize> {
    let Some(value) = value.filter(|value| python_truthy(value)) else {
        return Ok(default);
    };
    let parsed = match value {
        Value::Bool(true) => 1_i128,
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                i128::from(value)
            } else if let Some(value) = number.as_u64() {
                i128::from(value)
            } else if let Some(value) = number.as_f64() {
                let truncated = value.trunc();
                if !value.is_finite()
                    || truncated < i128::MIN as f64
                    || truncated > i128::MAX as f64
                {
                    anyhow::bail!("payload.max_tokens must be an integer");
                }
                truncated as i128
            } else {
                anyhow::bail!("payload.max_tokens must be an integer");
            }
        }
        Value::String(value) => value
            .trim()
            .parse::<i128>()
            .map_err(|_| anyhow::anyhow!("payload.max_tokens must be an integer"))?,
        _ => anyhow::bail!("payload.max_tokens must be an integer"),
    };
    if parsed < 1 {
        anyhow::bail!("payload.max_tokens must be positive");
    }
    usize::try_from(parsed).map_err(|_| anyhow::anyhow!("payload.max_tokens must be an integer"))
}

fn parse_temperature(value: Option<&Value>) -> anyhow::Result<f64> {
    let Some(value) = value.filter(|value| python_truthy(value)) else {
        return Ok(0.0);
    };
    let parsed = match value {
        Value::Bool(true) => 1.0,
        Value::Number(value) => value
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("payload.temperature must be a number"))?,
        Value::String(value) => value
            .trim()
            .parse::<f64>()
            .map_err(|_| anyhow::anyhow!("payload.temperature must be a number"))?,
        _ => anyhow::bail!("payload.temperature must be a number"),
    };
    if !parsed.is_finite() {
        anyhow::bail!("payload.temperature must be a finite number");
    }
    Ok(parsed)
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn openai_messages(payload: &Value) -> anyhow::Result<Vec<Value>> {
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("payload.messages must be an array of objects"))?;
    if messages.iter().any(|message| !message.is_object()) {
        anyhow::bail!("payload.messages must be an array of objects");
    }
    let mut output = Vec::new();
    let mut system_parts = Vec::new();
    if let Some(system) = payload.get("system") {
        let rendered = content_text(system);
        if !rendered.is_empty() {
            system_parts.push(rendered);
        }
    }
    for (message_index, message) in messages.iter().enumerate() {
        let role = required_string(
            message.get("role"),
            &format!("messages[{message_index}].role"),
        )?;
        let content = message.get("content").unwrap_or(&Value::Null);
        if matches!(role, "system" | "developer") {
            let rendered = content_text(content);
            if !rendered.is_empty() {
                system_parts.push(rendered);
            }
            continue;
        }
        if let Some(text) = content.as_str() {
            output.push(json!({"role": role, "content": text}));
            continue;
        }
        let blocks = content.as_array().ok_or_else(|| {
            anyhow::anyhow!("messages[{message_index}].content must be text or a block array")
        })?;
        if blocks.iter().any(|block| !block.is_object()) {
            anyhow::bail!("messages[{message_index}].content must be text or a block array");
        }
        if role == "assistant" {
            let text = content_text(content);
            let mut tool_calls = Vec::new();
            for (block_index, block) in blocks.iter().enumerate() {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let id = required_string(
                    block.get("id"),
                    &format!("messages[{message_index}].content[{block_index}].tool_use.id"),
                )?;
                let name = required_string(block.get("name"), &format!("tool_use {id:?} name"))?;
                let input = block
                    .get("input")
                    .filter(|input| input.is_object())
                    .ok_or_else(|| anyhow::anyhow!("tool_use {id:?} input must be an object"))?;
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": serde_json::to_string(input)?,
                    },
                }));
            }
            let mut item = json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { Value::String(text) },
            });
            if !tool_calls.is_empty() {
                item["tool_calls"] = Value::Array(tool_calls);
            }
            output.push(item);
            continue;
        }
        if role != "user" {
            anyhow::bail!("Unsupported Anthropic message role {role:?}");
        }
        let mut pending_text = Vec::new();
        for (block_index, block) in blocks.iter().enumerate() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => pending_text.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Some("tool_result") => {
                    if !pending_text.is_empty() {
                        output.push(json!({"role": "user", "content": pending_text.join("\n")}));
                        pending_text.clear();
                    }
                    let call_id = required_string(
                        block.get("tool_use_id"),
                        "tool_result.tool_use_id",
                    )?;
                    let result_content = block.get("content").unwrap_or(&Value::Null);
                    let rendered = content_text(result_content);
                    output.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": if rendered.is_empty() {
                            scalar_text(result_content)
                        } else {
                            rendered
                        },
                    }));
                }
                other => anyhow::bail!(
                    "Unsupported user content block {other:?} at messages[{message_index}].content[{block_index}]"
                ),
            }
        }
        if !pending_text.is_empty() {
            output.push(json!({"role": "user", "content": pending_text.join("\n")}));
        }
    }
    if !system_parts.is_empty() {
        output.insert(
            0,
            json!({"role": "system", "content": system_parts.join("\n\n")}),
        );
    }
    Ok(output)
}

fn openai_tools(payload: &Value) -> anyhow::Result<Vec<Value>> {
    let Some(raw_tools) = payload.get("tools") else {
        return Ok(Vec::new());
    };
    if raw_tools.is_null() {
        return Ok(Vec::new());
    }
    let raw_tools = raw_tools
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("payload.tools must be an array of objects"))?;
    let mut tools = Vec::new();
    for (index, tool) in raw_tools.iter().enumerate() {
        let name = required_string(tool.get("name"), &format!("tools[{index}].name"))?;
        let schema = tool
            .get("input_schema")
            .filter(|schema| !schema.is_null())
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !schema.is_object() {
            anyhow::bail!("tools[{index}].input_schema must be an object");
        }
        tools.push(json!({
            "type": "function",
            "function": {
                "name": name,
                "description": tool.get("description").and_then(Value::as_str).unwrap_or_default(),
                "parameters": schema,
            },
        }));
    }
    Ok(tools)
}

fn tool_choice(value: Option<&Value>) -> anyhow::Result<Option<Value>> {
    let Some(value) = value.and_then(Value::as_object) else {
        return Ok(None);
    };
    match value.get("type").and_then(Value::as_str) {
        Some("auto") => Ok(Some(json!("auto"))),
        Some("any") => Ok(Some(json!("required"))),
        Some("tool") => Ok(Some(json!({
            "type": "function",
            "function": {"name": required_string(value.get("name"), "tool_choice.name")?},
        }))),
        other => anyhow::bail!("Unsupported tool_choice type {other:?}"),
    }
}

fn anthropic_response(
    payload: &Value,
    upstream: &Value,
    model_name: &str,
) -> anyhow::Result<Value> {
    let choice = upstream
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .filter(|choice| choice.is_object())
        .ok_or_else(|| anyhow::anyhow!("OpenAI response has no first choice"))?;
    let message = choice
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| anyhow::anyhow!("OpenAI response choice has no message"))?;
    let mut content = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    let raw_tool_calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("OpenAI response tool_calls must be objects"))?
            .clone(),
    };
    if raw_tool_calls.iter().any(|call| !call.is_object()) {
        anyhow::bail!("OpenAI response tool_calls must be objects");
    }
    for (index, call) in raw_tool_calls.iter().enumerate() {
        let function = call
            .get("function")
            .filter(|function| function.is_object())
            .ok_or_else(|| anyhow::anyhow!("OpenAI tool_calls[{index}] has no function"))?;
        let arguments_text = match function.get("arguments") {
            None | Some(Value::Null) => "{}",
            Some(Value::String(value)) if value.is_empty() => "{}",
            Some(Value::String(value)) => value.as_str(),
            Some(_) => {
                anyhow::bail!("OpenAI tool_calls[{index}] arguments must be JSON text")
            }
        };
        let arguments: Value = serde_json::from_str(arguments_text).map_err(|_| {
            anyhow::anyhow!("OpenAI tool_calls[{index}] arguments are invalid JSON")
        })?;
        if !arguments.is_object() {
            anyhow::bail!("OpenAI tool_calls[{index}] arguments must decode to an object");
        }
        content.push(json!({
            "type": "tool_use",
            "id": required_string(call.get("id"), &format!("OpenAI tool_calls[{index}].id"))?,
            "name": required_string(function.get("name"), &format!("OpenAI tool_calls[{index}].function.name"))?,
            "input": arguments,
        }));
    }
    let stop_reason = if !raw_tool_calls.is_empty() {
        "tool_use"
    } else if choice.get("finish_reason").and_then(Value::as_str) == Some("length") {
        "max_tokens"
    } else {
        "end_turn"
    };
    let usage = upstream.get("usage").and_then(Value::as_object);
    Ok(json!({
        "id": upstream.get("id").and_then(Value::as_str).map(str::to_owned)
            .unwrap_or_else(|| format!("msg_{}", uuid::Uuid::new_v4().simple())),
        "type": "message",
        "role": "assistant",
        "model": payload.get("model").and_then(Value::as_str).unwrap_or(model_name),
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage.and_then(|value| value.get("prompt_tokens")).and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": usage.and_then(|value| value.get("completion_tokens")).and_then(Value::as_u64).unwrap_or(0),
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        },
    }))
}

fn sse_events(message: &Value) -> Vec<(&'static str, Value)> {
    let usage = message.get("usage").cloned().unwrap_or_else(|| json!({}));
    let mut start = message.clone();
    start["content"] = json!([]);
    start["stop_reason"] = Value::Null;
    start["usage"] = usage.clone();
    start["usage"]["output_tokens"] = json!(0);
    let mut events = vec![(
        "message_start",
        json!({"type": "message_start", "message": start}),
    )];
    for (index, block) in message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            events.push((
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {"type": "text", "text": ""},
                }),
            ));
            events.push((
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": block.get("text").cloned().unwrap_or(json!(""))},
                }),
            ));
        } else {
            events.push((
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": index,
                    "content_block": {
                        "type": "tool_use",
                        "id": block.get("id"),
                        "name": block.get("name"),
                        "input": {},
                    },
                }),
            ));
            events.push((
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {
                        "type": "input_json_delta",
                        "partial_json": serde_json::to_string(block.get("input").unwrap_or(&json!({}))).unwrap_or_else(|_| "{}".into()),
                    },
                }),
            ));
        }
        events.push((
            "content_block_stop",
            json!({"type": "content_block_stop", "index": index}),
        ));
    }
    events.push((
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": message.get("stop_reason"),
                "stop_sequence": Value::Null,
            },
            "usage": {"output_tokens": usage.get("output_tokens").cloned().unwrap_or(json!(0))},
        }),
    ));
    events.push(("message_stop", json!({"type": "message_stop"})));
    events
}

fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    let Some(values) = value.as_array() else {
        return String::new();
    };
    values
        .iter()
        .filter_map(|item| {
            if let Some(text) = item.as_str() {
                return Some(text.to_owned());
            }
            match item.get("type").and_then(Value::as_str) {
                Some("text") => Some(
                    item.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                ),
                Some("tool_result") => {
                    Some(content_text(item.get("content").unwrap_or(&Value::Null)))
                }
                _ => None,
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(false) => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(true) => "True".into(),
        Value::Number(value) if value.as_f64() == Some(0.0) => String::new(),
        Value::Number(value) => value.to_string(),
        Value::Array(value) if value.is_empty() => String::new(),
        Value::Object(value) if value.is_empty() => String::new(),
        other => other.to_string(),
    }
}

fn required_string<'a>(value: Option<&'a Value>, context: &str) -> anyhow::Result<&'a str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{context} must be a non-empty string"))
}

fn estimate_input_tokens(payload: &Value) -> usize {
    let rendered = serde_json::to_vec(payload).unwrap_or_default();
    rendered.len().div_ceil(3).max(1)
}

fn chat_completions_url(base: &str) -> Result<String, ReplayError> {
    let mut url = reqwest::Url::parse(base).map_err(|error| {
        ReplayError::configuration(format!("invalid OpenAI base URL {base:?}: {error}"))
    })?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/chat/completions") {
        path.to_owned()
    } else if path.is_empty() {
        "/v1/chat/completions".into()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

fn merged_no_proxy_environment() -> String {
    let configured = ["NO_PROXY", "no_proxy"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect::<Vec<_>>();
    merge_no_proxy_values(configured.iter().map(String::as_str))
}

fn merge_no_proxy_values<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let mut entries = Vec::<String>::new();
    for value in values {
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            if !entries.iter().any(|existing| existing == entry) {
                entries.push(entry.to_owned());
            }
        }
    }
    for required in ["127.0.0.1", "localhost", "::1"] {
        if !entries.iter().any(|existing| existing == required) {
            entries.push(required.into());
        }
    }
    entries.join(",")
}

fn first_nonempty_env(names: &[&'static str]) -> Option<(&'static str, String)> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (*name, value))
    })
}

fn integer_environment(name: &str, default: usize, minimum: usize) -> Result<usize, ReplayError> {
    let Some(raw) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(default);
    };
    let value = raw.parse::<usize>().map_err(|error| {
        ReplayError::configuration(format!("{name} must be an integer: {error}"))
    })?;
    if value < minimum {
        return Err(ReplayError::configuration(format!(
            "{name} must be >= {minimum}"
        )));
    }
    Ok(value)
}

fn float_environment(name: &str, default: f64, minimum: f64) -> Result<f64, ReplayError> {
    let Some(raw) = std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(default);
    };
    let value = raw
        .parse::<f64>()
        .map_err(|error| ReplayError::configuration(format!("{name} must be a number: {error}")))?;
    if !value.is_finite() || value < minimum {
        return Err(ReplayError::configuration(format!(
            "{name} must be a finite number >= {minimum}"
        )));
    }
    Ok(value)
}

fn retry_delays_environment() -> Result<Vec<Duration>, ReplayError> {
    let Some(raw) = std::env::var("SANDBOX_PLAYBACK_BRIDGE_RETRY_DELAYS_SECONDS")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(DEFAULT_RETRY_DELAYS_SECONDS
            .iter()
            .copied()
            .map(Duration::from_secs_f64)
            .collect());
    };
    raw.split(',')
        .map(|part| {
            let value = part.trim().parse::<f64>().map_err(|error| {
                ReplayError::configuration(format!(
                    "SANDBOX_PLAYBACK_BRIDGE_RETRY_DELAYS_SECONDS must contain numbers: {error}"
                ))
            })?;
            if !value.is_finite() || value < 0.0 {
                return Err(ReplayError::configuration(
                    "SANDBOX_PLAYBACK_BRIDGE_RETRY_DELAYS_SECONDS values must be finite and non-negative",
                ));
            }
            Ok(Duration::from_secs_f64(value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_user_prompt_is_injected_once_after_the_clean_prefix() {
        let mut first = json!({
            "messages": [
                {"role": "user", "content": "task"},
                {"role": "tool", "tool_call_id": "call-1", "content": "O-prime N"}
            ]
        });
        assert!(inject_boundary_user_prompt(&mut first, Some("review it"), 1).unwrap());
        assert_eq!(first["messages"].as_array().unwrap().len(), 3);
        assert_eq!(
            first["messages"][2],
            json!({"role": "user", "content": "review it"})
        );

        let mut later = first.clone();
        assert!(!inject_boundary_user_prompt(&mut later, Some("review it"), 2).unwrap());
        assert_eq!(later, first);

        let mut disabled = json!({"messages": []});
        assert!(!inject_boundary_user_prompt(&mut disabled, None, 1).unwrap());
        assert!(disabled["messages"].as_array().unwrap().is_empty());
    }

    #[test]
    fn openai_projection_matches_python_bridge_contract() {
        let payload = json!({
            "model": "claude",
            "max_tokens": 64,
            "system": [{"type":"text","text":"system"}],
            "messages": [
                {"role":"user","content":"task"},
                {"role":"assistant","content":[
                    {"type":"thinking","thinking":"hidden"},
                    {"type":"text","text":"run"},
                    {"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"pwd"}}
                ]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"call-1","content":"/workspace"}
                ]}
            ],
            "tools": [{"name":"Bash","description":"shell","input_schema":{"type":"object"}}]
        });
        let request = openai_request(&payload, "qwen", 8192, 200_000, 1024, true).unwrap();
        assert_eq!(request["model"], "qwen");
        assert_eq!(request["messages"][0]["role"], "system");
        assert_eq!(request["messages"][2]["tool_calls"][0]["id"], "call-1");
        assert_eq!(request["messages"][3]["role"], "tool");
        assert_eq!(request["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(request["reasoning_effort"], "none");
        assert!(request.get("stream").is_none());
    }

    #[test]
    fn openai_response_is_synthesized_as_anthropic_sse() {
        let payload = json!({"model":"claude","stream":true});
        let upstream = json!({
            "id":"chat-1",
            "choices":[{"finish_reason":"tool_calls","message":{
                "content":"working",
                "tool_calls":[{"id":"call-1","function":{"name":"Read","arguments":"{\"file_path\":\"/a\"}"}}]
            }}],
            "usage":{"prompt_tokens":12,"completion_tokens":5}
        });
        let message = anthropic_response(&payload, &upstream, "qwen").unwrap();
        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(message["content"][1]["input"]["file_path"], "/a");
        let events = sse_events(&message);
        assert_eq!(events.first().unwrap().0, "message_start");
        assert_eq!(events.last().unwrap().0, "message_stop");
    }

    #[test]
    fn base_url_accepts_root_v1_and_complete_endpoint() {
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8000").unwrap(),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8000/v1").unwrap(),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://127.0.0.1:8000/v1/chat/completions").unwrap(),
            "http://127.0.0.1:8000/v1/chat/completions"
        );
    }
    #[test]
    fn bridge_cleans_resume_transport_before_upstream_io() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            type Captured = Arc<Mutex<Option<(HeaderMap, Value)>>>;

            async fn capture(
                State(captured): State<Captured>,
                headers: HeaderMap,
                axum::Json(payload): axum::Json<Value>,
            ) -> axum::Json<Value> {
                *captured.lock().unwrap() = Some((headers, payload));
                axum::Json(json!({
                    "id": "chat-test",
                    "choices": [{
                        "finish_reason": "stop",
                        "message": {"role": "assistant", "content": "continued"}
                    }],
                    "usage": {"prompt_tokens": 31, "completion_tokens": 2}
                }))
            }

            let captured: Captured = Arc::new(Mutex::new(None));
            let upstream_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let upstream_address = upstream_listener.local_addr().unwrap();
            let upstream_router = Router::new()
                .route("/v1/chat/completions", post(capture))
                .with_state(Arc::clone(&captured));
            let upstream = tokio::spawn(async move {
                axum::serve(upstream_listener, upstream_router)
                    .await
                    .unwrap();
            });

            let canonical = vec![
                json!({"role": "user", "content": "task"}),
                json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "tool-1",
                        "name": "Bash",
                        "input": {"command": "pwd"}
                    }]
                }),
                json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "tool-1",
                        "content": "fresh observation"
                    }]
                }),
            ];
            let nonce = "__PVISOR_NATIVE_REPLAY_0123456789abcdef__".to_owned();
            let manifest = ResumeTransportManifest::create(
                "session-1",
                vec!["tool-1".into()],
                canonical.clone(),
                nonce.clone(),
            )
            .unwrap();
            let payload = json!({
                "model": "claude",
                "stream": true,
                "max_tokens": 64,
                "messages": [
                    canonical[0].clone(),
                    canonical[1].clone(),
                    {"role": "user", "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "tool-1",
                            "content": "fresh observation"
                        },
                        {"type": "text", "text": "Continue from where you left off."}
                    ]},
                    {"role": "assistant", "content": "No response requested."},
                    {"role": "user", "content": nonce},
                    {"role": "system", "content": "temporary suffix must be dropped"}
                ]
            });
            let shared = Arc::new(BridgeShared {
                resume: Mutex::new(ResumeState {
                    manifest,
                    request_sequence: 0,
                    forwarded_requests: 0,
                    pending_forward_sequence: None,
                    failed: false,
                    failure: None,
                }),
                client: reqwest::Client::builder().no_proxy().build().unwrap(),
                upstream_url: format!("http://{upstream_address}/v1/chat/completions"),
                upstream_api_key: "upstream-secret".into(),
                model_name: "qwen".into(),
                routing_session_id: "trial-session".into(),
                bridge_api_key: "local-key".into(),
                disable_thinking: true,
                boundary_user_prompt: None,
                max_output_tokens: 8192,
                model_context_tokens: 200_000,
                context_safety_tokens: 1024,
                upstream_timeout: Duration::from_secs(5),
                retry_delays: Vec::new(),
                cancelled: AtomicBool::new(false),
                cancel_notify: Notify::new(),
            });
            let mut headers = HeaderMap::new();
            headers.insert("x-api-key", HeaderValue::from_static("local-key"));
            let response = messages(
                State(Arc::clone(&shared)),
                headers,
                Bytes::from(payload.to_string()),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "text/event-stream"
            );
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let rendered = String::from_utf8(body.to_vec()).unwrap();
            assert!(rendered.contains("event: message_start"));
            assert!(rendered.contains("continued"));

            let (upstream_headers, upstream_payload) = captured.lock().unwrap().clone().unwrap();
            assert_eq!(
                upstream_headers
                    .get("X-LiteLLM-Session-ID")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "trial-session"
            );
            assert_eq!(upstream_payload["model"], "qwen");
            assert_eq!(
                upstream_payload["chat_template_kwargs"]["enable_thinking"],
                false
            );
            assert_eq!(upstream_payload["reasoning_effort"], "none");
            assert!(upstream_payload.get("stream").is_none());
            let messages = upstream_payload["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 3);
            assert_eq!(messages.last().unwrap()["role"], "tool");
            assert_eq!(messages.last().unwrap()["content"], "fresh observation");
            assert!(!upstream_payload
                .to_string()
                .contains("PVISOR_NATIVE_REPLAY"));
            assert!(!upstream_payload
                .to_string()
                .contains("temporary suffix must be dropped"));

            let resume = shared.resume.lock().unwrap();
            assert!(!resume.failed);
            assert_eq!(resume.request_sequence, 1);
            assert_eq!(resume.forwarded_requests, 1);
            assert_eq!(resume.pending_forward_sequence, None);
            drop(resume);
            upstream.abort();
        });
    }

    #[test]
    fn bridge_resume_state_stays_failed_after_one_rejection() {
        let canonical = vec![
            json!({"role": "user", "content": "task"}),
            json!({"role": "assistant", "content": [{
                "type": "tool_use", "id": "tool-1", "name": "Bash", "input": {}
            }]}),
            json!({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "tool-1", "content": "fresh"
            }]}),
        ];
        let manifest = ResumeTransportManifest::create(
            "session",
            vec!["tool-1".into()],
            canonical,
            "__PVISOR_NATIVE_REPLAY_0123456789abcdef__".into(),
        )
        .unwrap();
        let mut state = ResumeState {
            manifest,
            request_sequence: 0,
            forwarded_requests: 0,
            pending_forward_sequence: None,
            failed: false,
            failure: None,
        };
        assert!(state.clean(&json!({"messages": []})).is_err());
        assert!(state.failed);
        assert!(state
            .clean(&json!({"messages": []}))
            .unwrap_err()
            .to_string()
            .contains("FAILED"));
        assert_eq!(state.forwarded_requests, 0);
    }
    #[test]
    fn empty_openai_tool_arguments_mean_an_empty_object_but_non_text_is_rejected() {
        let payload = json!({"model": "claude"});
        let empty = json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call-1",
                "function": {"name": "TaskList", "arguments": ""}
            }]}}]
        });
        let converted = anthropic_response(&payload, &empty, "qwen").unwrap();
        assert_eq!(converted["content"][0]["input"], json!({}));

        let non_text = json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call-1",
                "function": {"name": "TaskList", "arguments": {}}
            }]}}]
        });
        assert!(anthropic_response(&payload, &non_text, "qwen")
            .unwrap_err()
            .to_string()
            .contains("must be JSON text"));
    }
    fn test_manifest() -> ResumeTransportManifest {
        let canonical = vec![
            json!({"role": "assistant", "content": [{
                "type": "tool_use", "id": "tool-1", "name": "Bash", "input": {}
            }]}),
            json!({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "tool-1", "content": "fresh"
            }]}),
        ];
        ResumeTransportManifest::create(
            "session",
            vec!["tool-1".into()],
            canonical,
            "__PVISOR_NATIVE_REPLAY_0123456789abcdef__".into(),
        )
        .unwrap()
    }

    fn test_shared(upstream_url: String, retry_delays: Vec<Duration>) -> Arc<BridgeShared> {
        Arc::new(BridgeShared {
            resume: Mutex::new(ResumeState {
                manifest: test_manifest(),
                request_sequence: 0,
                forwarded_requests: 0,
                pending_forward_sequence: None,
                failed: false,
                failure: None,
            }),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            upstream_url,
            upstream_api_key: "upstream-secret".into(),
            model_name: "qwen".into(),
            routing_session_id: "trial-session".into(),
            bridge_api_key: "local-key".into(),
            disable_thinking: false,
            boundary_user_prompt: None,
            max_output_tokens: 8192,
            model_context_tokens: 200_000,
            context_safety_tokens: 1024,
            upstream_timeout: Duration::from_secs(60),
            retry_delays,
            cancelled: AtomicBool::new(false),
            cancel_notify: Notify::new(),
        })
    }

    #[test]
    fn bridge_version_matches_python_contract_exactly() {
        assert_eq!(BRIDGE_VERSION, "sandbox-replay-anthropic-openai-bridge/1");
    }

    #[test]
    fn resume_state_preserves_the_first_failure_for_pending_concurrency() {
        let mut state = ResumeState {
            manifest: test_manifest(),
            request_sequence: 1,
            forwarded_requests: 0,
            pending_forward_sequence: Some(1),
            failed: false,
            failure: None,
        };
        let error = state.clean(&json!({"messages": []})).unwrap_err();
        assert!(error.to_string().contains("previous validated request"));
        assert!(state.failed);
        assert_eq!(
            state.failure.as_deref(),
            Some("another request arrived before the previous request was forwarded")
        );
        state.fail("later failure must not hide the root cause");
        assert_eq!(
            state.failure.as_deref(),
            Some("another request arrived before the previous request was forwarded")
        );
    }

    #[test]
    fn invalid_present_sampling_values_are_rejected() {
        let mut payload = json!({"messages": []});
        payload["max_tokens"] = json!("not-an-integer");
        assert!(openai_request(&payload, "qwen", 8192, 200_000, 1024, false)
            .unwrap_err()
            .to_string()
            .contains("max_tokens must be an integer"));

        payload = json!({"messages": [], "max_tokens": -1});
        assert!(openai_request(&payload, "qwen", 8192, 200_000, 1024, false)
            .unwrap_err()
            .to_string()
            .contains("max_tokens must be positive"));

        payload = json!({"messages": [], "temperature": {"bad": true}});
        assert!(openai_request(&payload, "qwen", 8192, 200_000, 1024, false)
            .unwrap_err()
            .to_string()
            .contains("temperature must be a number"));

        payload = json!({"messages": [], "max_tokens": "32", "temperature": "0.25"});
        let converted = openai_request(&payload, "qwen", 8192, 200_000, 1024, false).unwrap();
        assert_eq!(converted["max_tokens"], 32);
        assert_eq!(converted["temperature"], 0.25);
    }

    #[test]
    fn scalar_tool_result_text_matches_python_truthiness() {
        assert_eq!(scalar_text(&Value::Null), "");
        assert_eq!(scalar_text(&json!(false)), "");
        assert_eq!(scalar_text(&json!(0)), "");
        assert_eq!(scalar_text(&json!([])), "");
        assert_eq!(scalar_text(&json!({})), "");
        assert_eq!(scalar_text(&json!(true)), "True");
        assert_eq!(scalar_text(&json!(["value"])), "[\"value\"]");
    }

    #[test]
    fn no_proxy_merge_preserves_values_and_guarantees_loopback() {
        let merged =
            merge_no_proxy_values(["example.internal, localhost", "10.0.0.0/8,example.internal"]);
        assert_eq!(
            merged,
            "example.internal,localhost,10.0.0.0/8,127.0.0.1,::1"
        );
    }

    #[test]
    fn active_upstream_send_is_cancelled_promptly() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            async fn hang(State(started): State<Arc<Notify>>) -> Response {
                started.notify_one();
                std::future::pending::<Response>().await
            }

            let started = Arc::new(Notify::new());
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server_started = Arc::clone(&started);
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/v1/chat/completions", post(hang))
                        .with_state(server_started),
                )
                .await
                .unwrap();
            });
            let shared = test_shared(format!("http://{address}/v1/chat/completions"), Vec::new());
            let request_shared = Arc::clone(&shared);
            let request = tokio::spawn(async move {
                forward_openai_request(&request_shared, b"{}".to_vec()).await
            });
            tokio::time::timeout(Duration::from_secs(2), started.notified())
                .await
                .unwrap();
            shared.cancelled.store(true, Ordering::Release);
            shared.cancel_notify.notify_waiters();
            let error = tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .expect("cancelled request should return promptly")
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("cancelled during shutdown"));
            server.abort();
        });
    }

    #[test]
    fn retry_sleep_is_cancelled_promptly() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            async fn unavailable(State(started): State<Arc<Notify>>) -> Response {
                started.notify_one();
                upstream_error(StatusCode::SERVICE_UNAVAILABLE, "retry".into())
            }

            let started = Arc::new(Notify::new());
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server_started = Arc::clone(&started);
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    Router::new()
                        .route("/v1/chat/completions", post(unavailable))
                        .with_state(server_started),
                )
                .await
                .unwrap();
            });
            let shared = test_shared(
                format!("http://{address}/v1/chat/completions"),
                vec![Duration::from_secs(60)],
            );
            let request_shared = Arc::clone(&shared);
            let request = tokio::spawn(async move {
                forward_openai_request(&request_shared, b"{}".to_vec()).await
            });
            tokio::time::timeout(Duration::from_secs(2), started.notified())
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            shared.cancelled.store(true, Ordering::Release);
            shared.cancel_notify.notify_waiters();
            let error = tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .expect("retry sleep should stop promptly")
                .unwrap()
                .unwrap_err();
            assert!(error.to_string().contains("cancelled during shutdown"));
            server.abort();
        });
    }
}
