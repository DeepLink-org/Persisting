//! End-to-end HTTP checks for Harbor-style `[network]` egress policy.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use persisting_agentctl::{
    ControlController, ControlReason, ControlRequest, ControlTransition, PolicyControlController,
};
use persisting_gateway::config::ProxyConfig;
use persisting_gateway::sink::SeqOnlySink;
use persisting_gateway::{serve_with_runtime_control, serve_with_shutdown_and_ready};
use tokio::sync::oneshot;

struct DenyModelController;

impl ControlController for DenyModelController {
    fn authorize(&self, request: ControlRequest<'_>) -> ControlTransition {
        match request {
            ControlRequest::Model { .. } => {
                ControlTransition::denied(ControlReason::ModelNotAllowed)
            }
            network => PolicyControlController.authorize(network),
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn spawn_mock_http() -> (u16, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let redirect_target = format!("http://127.0.0.1:{port}/");
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .route(
                "/bytes/{size}",
                get(|Path(size): Path<usize>| async move { vec![b'x'; size.min(1_048_576)] }),
            )
            .route(
                "/redirect",
                get(move || {
                    let target = redirect_target.clone();
                    async move { (StatusCode::FOUND, [(axum::http::header::LOCATION, target)]) }
                }),
            )
            .route(
                "/v1/chat/completions",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "id": "chatcmpl-test",
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "hi"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }),
            )
            .route(
                "/echo-size",
                post(|body: axum::body::Bytes| async move { body.len().to_string() }),
            );
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .ok();
    });
    tokio::task::yield_now().await;
    (port, stop_tx)
}

async fn spawn_chunked_llm_http() -> (u16, oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                let chunks = futures_util::stream::iter([
                    Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(
                        br#"{"id":"chatcmpl-chunked","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"#,
                    )),
                    Ok(axum::body::Bytes::from_static(
                        br#""chunked"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                    )),
                ]);
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("connection", "x-upstream-hop")
                    .header("x-upstream-hop", "must-not-cross-proxy")
                    .body(axum::body::Body::from_stream(chunks))
                    .unwrap()
            }),
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .ok();
    });
    tokio::task::yield_now().await;
    (port, stop_tx)
}

async fn spawn_capturing_llm_http() -> (
    u16,
    Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    oneshot::Sender<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(std::sync::Mutex::new(None));
    let captured_for_app = Arc::clone(&captured);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |body: axum::body::Bytes| {
                let captured = Arc::clone(&captured_for_app);
                async move {
                    *captured.lock().unwrap() = serde_json::from_slice(&body).ok();
                    axum::Json(serde_json::json!({
                        "id": "chatcmpl-bridge",
                        "object": "chat.completion",
                        "model": "upstream-model",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "bridged"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4}
                    }))
                }
            }),
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .ok();
    });
    tokio::task::yield_now().await;
    (port, captured, stop_tx)
}

async fn spawn_capturing_gemini_http() -> (
    u16,
    Arc<std::sync::Mutex<Option<serde_json::Value>>>,
    oneshot::Sender<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let captured = Arc::new(std::sync::Mutex::new(None));
    let captured_for_app = Arc::clone(&captured);
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let app = Router::new().fallback(post(
            move |uri: Uri, headers: HeaderMap, body: axum::body::Bytes| {
                let captured = Arc::clone(&captured_for_app);
                async move {
                    let body: serde_json::Value =
                        serde_json::from_slice(&body).expect("Gemini request JSON");
                    *captured.lock().unwrap() = Some(serde_json::json!({
                        "path": uri.path(),
                        "query": uri.query(),
                        "api_key": headers
                            .get("x-goog-api-key")
                            .and_then(|value| value.to_str().ok()),
                        "authorization": headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok()),
                        "body": body,
                    }));
                    if uri.path().ends_with(":streamGenerateContent") {
                        Response::builder()
                            .status(StatusCode::OK)
                            .header("content-type", "text/event-stream")
                            .body(axum::body::Body::from(
                                "data: {\"responseId\":\"gemini-stream-e2e\",\"modelVersion\":\"gemini-2.5-pro\",\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"native streamed\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":3,\"candidatesTokenCount\":2,\"totalTokenCount\":5}}\n\n",
                            ))
                            .unwrap()
                            .into_response()
                    } else {
                        axum::Json(serde_json::json!({
                            "responseId": "gemini-e2e",
                            "modelVersion": "gemini-2.5-pro",
                            "candidates": [{
                                "index": 0,
                                "content": {
                                    "role": "model",
                                    "parts": [{"text": "native bridged"}]
                                },
                                "finishReason": "STOP"
                            }],
                            "usageMetadata": {
                                "promptTokenCount": 3,
                                "candidatesTokenCount": 2,
                                "totalTokenCount": 5
                            }
                        }))
                        .into_response()
                    }
                }
            },
        ));
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
            .ok();
    });
    tokio::task::yield_now().await;
    (port, captured, stop_tx)
}

async fn spawn_proxy(toml: &str) -> (String, tempfile::TempDir, oneshot::Sender<()>) {
    let listen_port = free_port();
    let admin_port = free_port();
    let toml = toml
        .replace("{{LISTEN}}", &format!("127.0.0.1:{listen_port}"))
        .replace("{{ADMIN}}", &format!("127.0.0.1:{admin_port}"));
    let cfg = ProxyConfig::from_toml_str(&toml).expect("proxy toml");
    let tmp = tempfile::tempdir().unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let storage = tmp.path().to_path_buf();
    let sink: Arc<dyn persisting_gateway::sink::CaptureEventSink> = Arc::new(SeqOnlySink::new());
    tokio::spawn(async move {
        let _ =
            serve_with_shutdown_and_ready(cfg, storage, sink, false, Some(ready_tx), async move {
                let _ = stop_rx.await;
            })
            .await;
    });
    ready_rx.await.expect("proxy ready");
    (format!("http://127.0.0.1:{listen_port}"), tmp, stop_tx)
}

async fn spawn_proxy_with_controller(
    toml: &str,
    controller: Arc<dyn ControlController>,
) -> (String, tempfile::TempDir, oneshot::Sender<()>) {
    let listen_port = free_port();
    let admin_port = free_port();
    let toml = toml
        .replace("{{LISTEN}}", &format!("127.0.0.1:{listen_port}"))
        .replace("{{ADMIN}}", &format!("127.0.0.1:{admin_port}"));
    let cfg = ProxyConfig::from_toml_str(&toml).expect("proxy toml");
    let tmp = tempfile::tempdir().unwrap();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let storage = tmp.path().to_path_buf();
    let sink: Arc<dyn persisting_gateway::sink::CaptureEventSink> = Arc::new(SeqOnlySink::new());
    tokio::spawn(async move {
        let _ = serve_with_runtime_control(
            cfg,
            storage,
            sink,
            false,
            controller,
            Some(ready_tx),
            async move {
                let _ = stop_rx.await;
            },
        )
        .await;
    });
    ready_rx.await.expect("proxy ready");
    (format!("http://127.0.0.1:{listen_port}"), tmp, stop_tx)
}

async fn raw_connect(proxy: &str, authority: &str) -> (StatusCode, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr: SocketAddr = proxy
        .trim_start_matches("http://")
        .parse()
        .expect("proxy addr");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 2048];
    let n = stream.read(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf[..n]);
    let status_line = text.lines().next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

async fn tunneled_get(proxy: &str, authority: &str, path: &str) -> (Duration, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let addr: SocketAddr = proxy.trim_start_matches("http://").parse().unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let connect = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(connect.as_bytes()).await.unwrap();
    let mut headers = Vec::new();
    let mut byte = [0_u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        headers.push(byte[0]);
        assert!(headers.len() < 8_192, "CONNECT response headers too large");
    }
    assert!(
        String::from_utf8_lossy(&headers).starts_with("HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&headers)
    );

    let started = tokio::time::Instant::now();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut response))
        .await
        .expect("tunneled response timeout")
        .unwrap();
    (started.elapsed(), response)
}

#[tokio::test]
async fn e2e_allowlist_denies_connect_to_unlisted_host() {
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "pypi.org",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let (status, body) = raw_connect(&proxy, "github.com:443").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("github.com"), "{body}");
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_allowlist_permits_connect_to_listed_host() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "127.0.0.1",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let (status, _) = raw_connect(&proxy, &format!("127.0.0.1:{mock_port}")).await;
    assert_eq!(status, StatusCode::OK);
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_model_upstream_is_not_an_agent_egress_grant() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = ["pypi.org"]

[[models]]
name = "*"
upstream = "http://127.0.0.1:{mock_port}/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;

    let (status, body) = raw_connect(&proxy, &format!("127.0.0.1:{mock_port}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("not-in-allowlist"), "{body}");
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_structured_rule_scopes_port_and_transport() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"

[[network.rules]]
host = "127.0.0.1"
ports = [{mock_port}]
transports = ["tcp_tunnel"]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;

    let (allowed, _) = raw_connect(&proxy, &format!("127.0.0.1:{mock_port}")).await;
    assert_eq!(allowed, StatusCode::OK);

    let (wrong_port, body) = raw_connect(&proxy, "127.0.0.1:1").await;
    assert_eq!(wrong_port, StatusCode::FORBIDDEN);
    assert!(body.contains("port-not-allowed"), "{body}");

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://127.0.0.1:{mock_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("transport-not-allowed")
    );
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_connect_does_not_report_success_before_upstream_connects() {
    let closed_port = free_port();
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let (status, body) = raw_connect(&proxy, &format!("127.0.0.1:{closed_port}")).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("CONNECT"), "{body}");
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_malformed_connect_authorities_fail_closed() {
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;
    for authority in ["example.com:0", "example.com:65536", "user@example.com:443"] {
        let (status, _) = raw_connect(&proxy, authority).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{authority}");
    }
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_no_network_denies_connect_including_loopback() {
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "no-network"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let (denied, _) = raw_connect(&proxy, "example.com:443").await;
    assert_eq!(denied, StatusCode::FORBIDDEN);
    let (loopback, _) = raw_connect(&proxy, "127.0.0.1:9").await;
    assert_eq!(loopback, StatusCode::FORBIDDEN);
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_explicit_deny_overrides_public_default() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[network.deny_rules]]
host = "127.0.0.1"
ports = [{mock_port}]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let (status, body) = raw_connect(&proxy, &format!("127.0.0.1:{mock_port}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("explicit-deny"), "{body}");
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_cidr_deny_filters_hostname_dns_results() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[network.deny_rules]]
host = "127.0.0.0/8"

[[network.deny_rules]]
host = "::1/128"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#
    .to_string();
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let (status, body) = raw_connect(&proxy, &format!("localhost:{mock_port}")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("explicit-deny"), "{body}");
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_global_bandwidth_limit_throttles_forwarded_body() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[network.limits]]
bytes_per_second = 32768

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let started = tokio::time::Instant::now();
    let response = client
        .get(format!("http://127.0.0.1:{mock_port}/bytes/32768"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.bytes().await.unwrap().len(), 32768);
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "bandwidth limit completed too quickly: {:?}",
        started.elapsed()
    );
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_bandwidth_limit_throttles_http_upload() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[network.limits]]
bytes_per_second = 32768

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let started = tokio::time::Instant::now();
    let response = client
        .post(format!("http://127.0.0.1:{mock_port}/echo-size"))
        .body(vec![b'x'; 32768])
        .send()
        .await
        .unwrap();
    assert_eq!(response.text().await.unwrap(), "32768");
    assert!(started.elapsed() >= Duration::from_millis(800));
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_bandwidth_limit_is_shared_by_concurrent_requests() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[network.limits]]
bytes_per_second = 32768

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(8))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{mock_port}/bytes/16384");
    let started = tokio::time::Instant::now();
    let (first, second) = tokio::join!(client.get(&url).send(), client.get(&url).send());
    let (first, second) = (first.unwrap(), second.unwrap());
    assert_eq!(first.bytes().await.unwrap().len(), 16384);
    assert_eq!(second.bytes().await.unwrap().len(), 16384);
    assert!(started.elapsed() >= Duration::from_millis(800));
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_bandwidth_limit_throttles_connect_tunnel_bytes() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[network.limits]]
bytes_per_second = 32768

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;
    let (elapsed, response) =
        tunneled_get(&proxy, &format!("127.0.0.1:{mock_port}"), "/bytes/32768").await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(response.ends_with(&vec![b'x'; 32768]));
    assert!(
        elapsed >= Duration::from_millis(800),
        "elapsed: {elapsed:?}"
    );
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_allowlist_denies_absolute_uri_forward() {
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "pypi.org",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client.get("http://github.com/").send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = resp.text().await.unwrap();
    assert!(body.contains("github.com"), "{body}");
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_allowlist_allows_absolute_uri_to_local_mock() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "127.0.0.1",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://127.0.0.1:{mock_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "ok");
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_hostname_rule_rejects_private_dns_result_by_default() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"

[[network.rules]]
host = "localhost"
ports = [{mock_port}]
transports = ["http"]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(format!("http://localhost:{mock_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("resolved-address-not-allowed")
    );
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_hostname_rule_can_explicitly_allow_private_dns_results() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"

[[network.rules]]
host = "localhost"
ports = [{mock_port}]
transports = ["http"]
allow_private_ips = true

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let response = client
        .get(format!("http://localhost:{mock_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "ok");
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_cross_host_redirect_returns_through_policy_gate() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"

[[network.rules]]
host = "localhost"
ports = [{mock_port}]
transports = ["http"]
allow_private_ips = true

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://localhost:{mock_port}/redirect"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.text().await.unwrap().contains("127.0.0.1"));
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_relative_llm_gateway_bypasses_host_allowlist() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "pypi.org",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:{mock_port}/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;

    // No HTTP_PROXY: hit listen with a relative path (LLM gateway).
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    // Must not be blocked by network policy (403). Upstream mock returns 200.
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_non_streaming_chunked_upstream_returns_complete_json() {
    let (mock_port, mock_stop) = spawn_chunked_llm_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "*"
upstream = "http://127.0.0.1:{mock_port}/v1"
"#,
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;

    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("{proxy}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "test",
            "stream": false,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("x-upstream-hop").is_none());
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "chunked");

    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_messages_bridge_keeps_forwarding_and_translates_both_directions() {
    let (mock_port, captured, mock_stop) = spawn_capturing_llm_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "claude-client"
forward = "upstream-model"

[[models]]
name = "upstream-model"
upstream = "http://127.0.0.1:{mock_port}/v1"
"#
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let original = serde_json::json!({
        "model": "claude-client",
        "max_tokens": 32,
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [{
            "name": "shell",
            "description": "run a command",
            "input_schema": {"type": "object", "properties": {}}
        }]
    });

    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("{proxy}/v1/messages"))
        .header("content-type", "application/json")
        .json(&original)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let client_body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(client_body["type"], "message");
    assert_eq!(client_body["model"], "claude-client");
    assert_eq!(client_body["content"][0]["text"], "bridged");

    let upstream = captured.lock().unwrap().clone().expect("upstream request");
    assert_eq!(upstream["model"], "upstream-model");
    assert_eq!(upstream["max_completion_tokens"], 32);
    assert_eq!(upstream["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(upstream["tools"][0]["function"]["name"], "shell");

    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_messages_bridge_uses_gemini_native_forwarding_and_google_auth() {
    let (mock_port, captured, mock_stop) = spawn_capturing_gemini_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "claude-client"
forward = "gemini-2.5-pro"

[[models]]
name = "gemini-2.5-pro"
provider = "gemini"
upstream = "http://127.0.0.1:{mock_port}/v1beta"
api_key = "gemini-secret"
"#,
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("{proxy}/v1/messages"))
        .header("content-type", "application/json")
        .header("authorization", "Bearer client-secret")
        .json(&serde_json::json!({
            "model": "claude-client",
            "max_tokens": 32,
            "system": "Be concise.",
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let client_body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(client_body["type"], "message");
    assert_eq!(client_body["model"], "claude-client");
    assert_eq!(client_body["content"][0]["text"], "native bridged");

    let upstream = captured.lock().unwrap().clone().expect("upstream request");
    assert_eq!(
        upstream["path"],
        "/v1beta/models/gemini-2.5-pro:generateContent"
    );
    assert_eq!(upstream["query"], serde_json::Value::Null);
    assert_eq!(upstream["api_key"], "gemini-secret");
    assert_eq!(upstream["authorization"], serde_json::Value::Null);
    assert_eq!(upstream["body"]["contents"][0]["role"], "user");
    assert_eq!(upstream["body"]["contents"][0]["parts"][0]["text"], "hello");
    assert_eq!(
        upstream["body"]["systemInstruction"]["parts"][0]["text"],
        "Be concise."
    );
    assert_eq!(upstream["body"]["generationConfig"]["maxOutputTokens"], 32);

    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_completions_bridge_uses_gemini_native_stream_endpoint() {
    let (mock_port, captured, mock_stop) = spawn_capturing_gemini_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "gemini-2.5-pro"
provider = "gemini"
upstream = "http://127.0.0.1:{mock_port}/v1beta"
api_key = "gemini-secret"
"#,
    );
    let (proxy, _tmp, stop) = spawn_proxy(&toml).await;
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": "gemini-2.5-pro",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let client_sse = response.text().await.unwrap();
    assert!(client_sse.contains("chat.completion.chunk"), "{client_sse}");
    assert!(client_sse.contains("native streamed"), "{client_sse}");
    assert!(client_sse.contains("data: [DONE]"), "{client_sse}");

    let upstream = captured.lock().unwrap().clone().expect("upstream request");
    assert_eq!(
        upstream["path"],
        "/v1beta/models/gemini-2.5-pro:streamGenerateContent"
    );
    assert_eq!(upstream["query"], "alt=sse");
    assert_eq!(upstream["api_key"], "gemini-secret");
    assert!(upstream["body"].get("stream").is_none());

    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_injected_pvisor_controller_denies_model_before_upstream() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let toml = format!(
        r#"
listen = "{{{{LISTEN}}}}"
admin_listen = "{{{{ADMIN}}}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "*"
upstream = "http://127.0.0.1:{mock_port}/v1"
"#
    );
    let (proxy, _tmp, stop) =
        spawn_proxy_with_controller(&toml, Arc::new(DenyModelController)).await;

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .post(format!("{proxy}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"test","messages":[{"role":"user","content":"hi"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        resp.text().await.unwrap().contains("model-not-allowed"),
        "denial should expose the stable pVisor reason"
    );
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_public_mode_allows_absolute_uri_forward() {
    let (mock_port, mock_stop) = spawn_mock_http().await;
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "public"

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get(format!("http://127.0.0.1:{mock_port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = stop.send(());
    let _ = mock_stop.send(());
}

#[tokio::test]
async fn e2e_absolute_uri_llm_path_still_checks_host() {
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "pypi.org",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(&proxy).unwrap())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    // Absolute-URI LLM path to an unlisted host must be denied before capture/upstream.
    let resp = client
        .post("http://api.openai.com/v1/chat/completions")
        .header("content-type", "application/json")
        .body(r#"{"model":"gpt","messages":[{"role":"user","content":"x"}]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = resp.text().await.unwrap();
    assert!(body.contains("api.openai.com"), "{body}");
    let _ = stop.send(());
}
