//! End-to-end HTTP checks for Harbor-style `[network]` egress policy.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use persisting_capture::config::ProxyConfig;
use persisting_capture::proxy::serve_with_shutdown_and_ready;
use persisting_capture::sink::SeqOnlySink;
use tokio::sync::oneshot;

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
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let app = Router::new().route("/", get(|| async { "ok" })).route(
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
    let sink: Arc<dyn persisting_capture::sink::CaptureSink> = Arc::new(SeqOnlySink::new());
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
    let (proxy, _tmp, stop) = spawn_proxy(
        r#"
listen = "{{LISTEN}}"
admin_listen = "{{ADMIN}}"
agent_id = "t"

[network]
mode = "allowlist"
allowed_hosts = [
    "example.com",
]

[[models]]
name = "*"
upstream = "http://127.0.0.1:9/v1"
"#,
    )
    .await;

    let (status, _) = raw_connect(&proxy, "example.com:443").await;
    assert_eq!(status, StatusCode::OK);
    let _ = stop.send(());
}

#[tokio::test]
async fn e2e_no_network_denies_connect_but_allows_loopback() {
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
    let (ok, _) = raw_connect(&proxy, "127.0.0.1:9").await;
    assert_eq!(ok, StatusCode::OK);
    let _ = stop.send(());
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
