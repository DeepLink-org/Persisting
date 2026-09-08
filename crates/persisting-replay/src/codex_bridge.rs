//! Codex Responses API resume-transport bridge.
//!
//! Older Codex releases require a prompt argument for `exec resume`.  The
//! prompt is useful as a CLI wake-up signal, but it must not become part of
//! the model input for an unmodified replay.  This bridge removes the unique
//! nonce from every request before forwarding it upstream; Codex may resend
//! the full conversation history on subsequent requests.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use crate::error::{ReplayError, ReplayErrorKind, ResultExt};

const BRIDGE_VERSION: &str = "sandbox-replay-codex-responses-bridge/1";
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    TransportNonce,
    ExplicitUserPrompt,
}

impl PromptMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TransportNonce => "transport_nonce",
            Self::ExplicitUserPrompt => "explicit_user_prompt",
        }
    }
}

pub struct CodexBridgeHandle {
    pub base_url: String,
    api_key: String,
    shared: Arc<BridgeShared>,
    shutdown: Option<oneshot::Sender<()>>,
    worker_done: Option<mpsc::Receiver<anyhow::Result<()>>>,
    worker: Option<JoinHandle<()>>,
}

struct BridgeShared {
    state: Mutex<BridgeState>,
    client: reqwest::Client,
    upstream_url: String,
    upstream_api_key: String,
    routing_session_id: String,
    bridge_api_key: String,
    prompt_mode: PromptMode,
    transport_prompt: String,
    explicit_prompt: Option<String>,
    cancelled: AtomicBool,
    cancel_notify: Notify,
}

struct BridgeState {
    request_sequence: usize,
    forwarded_requests: usize,
    removed_transport_prompt: bool,
    pending_forward_sequence: Option<usize>,
    failed: bool,
    failure: Option<String>,
}

impl BridgeState {
    fn fail(&mut self, message: impl Into<String>) {
        self.failed = true;
        if self.failure.is_none() {
            self.failure = Some(message.into());
        }
    }
}

impl CodexBridgeHandle {
    pub fn start(
        routing_session_id: &str,
        transport_prompt: String,
        explicit_prompt: Option<&str>,
    ) -> Result<Self, ReplayError> {
        let upstream_base = first_nonempty_env(&[
            "OPENAI_BASE_URL",
            "OPENAI_API_BASE",
            "LLM_BASE_URL",
        ])
        .ok_or_else(|| {
            ReplayError::configuration(
                "Codex SandboxReplay bridge requires OPENAI_BASE_URL, OPENAI_API_BASE, or LLM_BASE_URL",
            )
        })?;
        let upstream_api_key =
            first_nonempty_env(&["OPENAI_API_KEY", "LLM_API_KEY"]).ok_or_else(|| {
                ReplayError::configuration(
                    "Codex SandboxReplay bridge requires OPENAI_API_KEY or LLM_API_KEY",
                )
            })?;
        let upstream_url = responses_url(&upstream_base)?;
        let prompt_mode = if explicit_prompt.is_some() {
            PromptMode::ExplicitUserPrompt
        } else {
            PromptMode::TransportNonce
        };
        let bridge_api_key = format!("pvisor-sandbox-replay-{}", uuid::Uuid::new_v4().simple());
        let listener = TcpListener::bind(("127.0.0.1", 0)).replay_context(
            ReplayErrorKind::Continuation,
            "allocate Codex SandboxReplay bridge port",
        )?;
        let address = listener.local_addr().replay_context(
            ReplayErrorKind::Continuation,
            "read Codex SandboxReplay bridge address",
        )?;
        listener.set_nonblocking(true).replay_context(
            ReplayErrorKind::Continuation,
            "configure Codex SandboxReplay bridge listener",
        )?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .replay_context(
                ReplayErrorKind::Continuation,
                "build Codex SandboxReplay bridge client",
            )?;
        let shared = Arc::new(BridgeShared {
            state: Mutex::new(BridgeState {
                request_sequence: 0,
                forwarded_requests: 0,
                removed_transport_prompt: false,
                pending_forward_sequence: None,
                failed: false,
                failure: None,
            }),
            client,
            upstream_url,
            upstream_api_key,
            routing_session_id: routing_session_id.to_owned(),
            bridge_api_key: bridge_api_key.clone(),
            prompt_mode,
            transport_prompt,
            explicit_prompt: explicit_prompt.map(str::to_owned),
            cancelled: AtomicBool::new(false),
            cancel_notify: Notify::new(),
        });
        let router = router(Arc::clone(&shared));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("pvisor-codex-replay-bridge".into())
            .spawn(move || {
                let result = run_worker(listener, router, shutdown_rx, ready_tx);
                let _ = done_tx.send(result);
            })
            .replay_context(
                ReplayErrorKind::Continuation,
                "start Codex SandboxReplay bridge thread",
            )?;
        let mut handle = Self {
            base_url: format!("http://{address}/v1"),
            api_key: bridge_api_key,
            shared,
            shutdown: Some(shutdown_tx),
            worker_done: Some(done_rx),
            worker: Some(worker),
        };
        let startup_error = match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => None,
            Ok(Err(message)) => Some(message),
            Err(mpsc::RecvTimeoutError::Timeout) => Some(format!(
                "Codex SandboxReplay bridge did not become ready within {} seconds",
                START_TIMEOUT.as_secs()
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Some("Codex SandboxReplay bridge exited before reporting readiness".into())
            }
        };
        if let Some(message) = startup_error {
            let _ = handle.stop_worker();
            return Err(ReplayError::continuation(message));
        }
        Ok(handle)
    }

    pub fn child_environment(&self) -> BTreeMap<String, String> {
        let no_proxy = merged_no_proxy_environment();
        BTreeMap::from([
            ("OPENAI_BASE_URL".into(), self.base_url.clone()),
            ("OPENAI_API_BASE".into(), self.base_url.clone()),
            ("OPENAI_API_KEY".into(), self.api_key.clone()),
            ("NO_PROXY".into(), no_proxy.clone()),
            ("no_proxy".into(), no_proxy),
        ])
    }

    pub fn prompt_mode(&self) -> PromptMode {
        self.shared.prompt_mode
    }

    pub fn finish(mut self) -> Result<usize, ReplayError> {
        self.stop_worker()?;
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| ReplayError::continuation("Codex bridge state lock poisoned"))?;
        if state.failed {
            return Err(ReplayError::continuation(format!(
                "Codex resume transport bridge failed closed: {}",
                state
                    .failure
                    .as_deref()
                    .unwrap_or("unknown protocol failure")
            )));
        }
        if state.pending_forward_sequence.is_some() {
            return Err(ReplayError::continuation(
                "Codex bridge has a validated request that was not forwarded",
            ));
        }
        if state.forwarded_requests == 0 {
            return Err(ReplayError::continuation(
                "Codex continuation made no validated model request through the SandboxReplay bridge",
            ));
        }
        if self.shared.prompt_mode == PromptMode::TransportNonce && !state.removed_transport_prompt
        {
            return Err(ReplayError::continuation(
                "Codex transport nonce was not removed from the first model request",
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
            Some(done) => match done.recv_timeout(STOP_TIMEOUT) {
                Ok(result) => Some(result),
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.worker.take();
                    return Err(ReplayError::continuation(format!(
                        "Codex SandboxReplay bridge did not stop within {} seconds",
                        STOP_TIMEOUT.as_secs()
                    )));
                }
            },
            None => None,
        };
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            return Err(ReplayError::continuation(
                "Codex SandboxReplay bridge thread panicked",
            ));
        }
        if let Some(result) = worker_result {
            result.replay_context(ReplayErrorKind::Executor, "stop Codex SandboxReplay bridge")?;
        }
        Ok(())
    }
}

impl Drop for CodexBridgeHandle {
    fn drop(&mut self) {
        let _ = self.stop_worker();
    }
}

fn run_worker(
    listener: TcpListener,
    router: Router,
    shutdown_rx: oneshot::Receiver<()>,
    ready_tx: mpsc::SyncSender<std::result::Result<(), String>>,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| {
            let _ = ready_tx.send(Err(format!("build Codex bridge runtime: {error}")));
            error
        })?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener).map_err(|error| {
            let _ = ready_tx.send(Err(format!("adopt Codex bridge listener: {error}")));
            error
        })?;
        ready_tx
            .send(Ok(()))
            .map_err(|_| anyhow::anyhow!("Codex bridge startup receiver was dropped"))?;
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await?;
        anyhow::Ok(())
    })
}

fn router(shared: Arc<BridgeShared>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/responses", post(responses))
        .route("/v1/responses", post(responses))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(shared)
}

async fn health(State(shared): State<Arc<BridgeShared>>) -> Response {
    let (failed, request_sequence) = shared
        .state
        .lock()
        .map(|state| (state.failed, state.request_sequence))
        .unwrap_or((true, 0));
    response(
        StatusCode::OK.as_u16(),
        "application/json",
        json!({
            "status": "healthy",
            "bridge_version": BRIDGE_VERSION,
            "resume_mode": true,
            "resume_failed": failed,
            "request_sequence": request_sequence,
        })
        .to_string()
        .into_bytes(),
    )
}

async fn responses(
    State(shared): State<Arc<BridgeShared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&shared, &headers) {
        return error_response(StatusCode::UNAUTHORIZED, "invalid bridge API key");
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(Value::Object(payload)) => Value::Object(payload),
        Ok(_) => return error_response(StatusCode::BAD_REQUEST, "request must be a JSON object"),
        Err(error) => {
            return error_response(StatusCode::BAD_REQUEST, &format!("invalid JSON: {error}"));
        }
    };
    let (cleaned, sequence) = match clean_request(&shared, payload) {
        Ok(result) => result,
        Err(error) => {
            fail(&shared, error.to_string());
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string());
        }
    };
    let serialized = match serde_json::to_vec(&cleaned) {
        Ok(serialized) => serialized,
        Err(error) => {
            fail(&shared, format!("serialize cleaned Codex request: {error}"));
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, &error.to_string());
        }
    };
    let upstream = match forward(&shared, serialized).await {
        Ok(response) => response,
        Err(error) => {
            fail(&shared, error.to_string());
            return error_response(StatusCode::BAD_GATEWAY, &error.to_string());
        }
    };
    {
        let mut state = match shared.state.lock() {
            Ok(state) => state,
            Err(_) => {
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "bridge state lock poisoned",
                );
            }
        };
        if state.pending_forward_sequence != Some(sequence) {
            state.fail("forwarded Codex request sequence did not match the validated request");
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bridge request sequence mismatch",
            );
        }
        state.pending_forward_sequence = None;
        state.forwarded_requests += 1;
    }
    response_with_headers(upstream.status, upstream.headers, upstream.body)
}

struct UpstreamResponse {
    status: u16,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn forward(shared: &BridgeShared, body: Vec<u8>) -> anyhow::Result<UpstreamResponse> {
    if shared.cancelled.load(Ordering::Acquire) {
        anyhow::bail!("Codex SandboxReplay bridge was cancelled");
    }
    let request = shared
        .client
        .post(&shared.upstream_url)
        .header(
            AUTHORIZATION.as_str(),
            format!("Bearer {}", shared.upstream_api_key),
        )
        .header(CONTENT_TYPE.as_str(), "application/json")
        .header("X-LiteLLM-Session-ID", &shared.routing_session_id)
        .body(body)
        .send()
        .await?;
    let status = request.status().as_u16();
    let headers: HeaderMap = request
        .headers()
        .iter()
        .filter(|(name, _)| !is_hop_by_hop_header(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok(UpstreamResponse {
        status,
        headers,
        body: request.bytes().await?.to_vec(),
    })
}

fn clean_request(shared: &Arc<BridgeShared>, mut payload: Value) -> anyhow::Result<(Value, usize)> {
    let mut state = shared
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("Codex bridge state lock poisoned"))?;
    if state.failed {
        anyhow::bail!(
            "Codex bridge is failed closed: {}",
            state.failure.as_deref().unwrap_or("unknown failure")
        );
    }
    if state.pending_forward_sequence.is_some() {
        state.fail("another Codex request arrived before the previous request was forwarded");
        anyhow::bail!("another request arrived before the previous request was forwarded");
    }
    state.request_sequence += 1;
    let sequence = state.request_sequence;
    let input = payload
        .get_mut("input")
        .context("Codex Responses request has no input")?;
    match shared.prompt_mode {
        PromptMode::TransportNonce => {
            let removed = remove_exact_user_input(input, &shared.transport_prompt)?;
            if sequence == 1 && removed != 1 {
                state.fail(format!(
                    "expected exactly one Codex transport nonce in the first request, found {removed}"
                ));
                anyhow::bail!(
                    "expected exactly one Codex transport nonce in the first request, found {removed}"
                );
            }
            if sequence == 1 {
                state.removed_transport_prompt = true;
            }
        }
        PromptMode::ExplicitUserPrompt => {
            if sequence == 1 {
                let prompt = shared
                    .explicit_prompt
                    .as_deref()
                    .context("explicit Codex prompt mode has no prompt")?;
                let count = count_exact_user_input(input, prompt)?;
                if count != 1 {
                    state.fail(format!(
                        "expected exactly one explicit Codex boundary prompt in the first request, found {count}"
                    ));
                    anyhow::bail!(
                        "expected exactly one explicit Codex boundary prompt in the first request, found {count}"
                    );
                }
            }
        }
    }
    state.pending_forward_sequence = Some(sequence);
    Ok((payload, sequence))
}

fn remove_exact_user_input(input: &mut Value, expected: &str) -> anyhow::Result<usize> {
    let items = input
        .as_array_mut()
        .context("Codex Responses input must be an array for resume transport cleanup")?;
    let mut matches = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if item.get("role").and_then(Value::as_str) == Some("user")
            && exact_message_text(item) == Some(expected)
        {
            matches.push(index);
        }
    }
    if matches.len() > 1 {
        anyhow::bail!("Codex transport nonce occurred more than once in input");
    }
    if let Some(index) = matches.first().copied() {
        items.remove(index);
        Ok(1)
    } else {
        Ok(0)
    }
}

fn count_exact_user_input(input: &Value, expected: &str) -> anyhow::Result<usize> {
    let items = input
        .as_array()
        .context("Codex Responses input must be an array for resume transport validation")?;
    Ok(items
        .iter()
        .filter(|item| {
            item.get("role").and_then(Value::as_str) == Some("user")
                && exact_message_text(item) == Some(expected)
        })
        .count())
}

fn exact_message_text(message: &Value) -> Option<&str> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text);
    }
    let blocks = content.as_array()?;
    if blocks.len() != 1 {
        return None;
    }
    let block = &blocks[0];
    if block.get("type").and_then(Value::as_str) != Some("input_text") {
        return None;
    }
    block.get("text").and_then(Value::as_str)
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

fn fail(shared: &BridgeShared, message: String) {
    if let Ok(mut state) = shared.state.lock() {
        state.fail(message);
    }
}

fn response(status: u16, content_type: &str, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Ok(value) = HeaderValue::from_str(content_type) {
        response.headers_mut().insert(CONTENT_TYPE, value);
    }
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn response_with_headers(status: u16, headers: HeaderMap, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    *response.headers_mut() = headers;
    response
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
    )
}

fn error_response(status: StatusCode, message: &str) -> Response {
    response(
        status.as_u16(),
        "application/json",
        json!({"error": {"message": message}})
            .to_string()
            .into_bytes(),
    )
}

async fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "not found")
}

fn responses_url(base: &str) -> Result<String, ReplayError> {
    let mut url = reqwest::Url::parse(base).map_err(|error| {
        ReplayError::configuration(format!("invalid OpenAI base URL {base:?}: {error}"))
    })?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/responses") {
        path.to_owned()
    } else if path.ends_with("/v1") {
        format!("{path}/responses")
    } else if path.is_empty() {
        "/v1/responses".into()
    } else {
        format!("{path}/v1/responses")
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
    let mut entries = Vec::new();
    for value in configured {
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

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_url_appends_responses_endpoint() {
        assert_eq!(
            responses_url("http://model/v1").unwrap(),
            "http://model/v1/responses"
        );
        assert_eq!(
            responses_url("http://model/v1/").unwrap(),
            "http://model/v1/responses"
        );
        assert_eq!(
            responses_url("http://model").unwrap(),
            "http://model/v1/responses"
        );
    }

    #[test]
    fn hop_by_hop_response_headers_are_not_forwarded() {
        assert!(is_hop_by_hop_header("connection"));
        assert!(is_hop_by_hop_header("Transfer-Encoding"));
        assert!(is_hop_by_hop_header("content-length"));
        assert!(!is_hop_by_hop_header("content-type"));
        assert!(!is_hop_by_hop_header("date"));
    }

    #[test]
    fn removes_exact_nonce_from_responses_input() {
        let mut input = json!([
            {"role":"user","content":[{"type":"input_text","text":"task"}]},
            {"role":"user","content":[{"type":"input_text","text":"nonce-1234567890"}]},
            {"role":"assistant","content":[]}
        ]);
        assert_eq!(
            remove_exact_user_input(&mut input, "nonce-1234567890").unwrap(),
            1
        );
        assert_eq!(input.as_array().unwrap().len(), 2);
        assert_eq!(
            count_exact_user_input(&input, "nonce-1234567890").unwrap(),
            0
        );
    }

    #[test]
    fn nonce_must_be_unique() {
        let mut input = json!([
            {"role":"user","content":"nonce"},
            {"role":"user","content":"nonce"}
        ]);
        assert!(remove_exact_user_input(&mut input, "nonce").is_err());
    }

    #[test]
    fn historical_nonce_is_removed_from_later_requests() {
        let nonce = "pvisor-codex-resume-test";
        let mut first = json!({
            "input": [
                {"role": "user", "content": [{"type": "input_text", "text": nonce}]},
                {"role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}
            ]
        });
        let mut second = first.clone();
        let removed_first =
            remove_exact_user_input(first.get_mut("input").unwrap(), nonce).unwrap();
        let removed_second =
            remove_exact_user_input(second.get_mut("input").unwrap(), nonce).unwrap();
        assert_eq!(removed_first, 1);
        assert_eq!(removed_second, 1);
        assert!(!first.to_string().contains(nonce));
        assert!(!second.to_string().contains(nonce));
    }

    #[test]
    fn explicit_prompt_is_not_removed() {
        let input = json!([
            {"role":"user","content":"task"},
            {"role":"user","content":"review O-prime N"}
        ]);
        assert_eq!(
            count_exact_user_input(&input, "review O-prime N").unwrap(),
            1
        );
    }
}
