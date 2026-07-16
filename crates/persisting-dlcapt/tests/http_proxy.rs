use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use http_body_util::BodyExt;
use persisting_dlcapt::config::{ExportConfig, ModelRoute, ProxyConfig, StorageConfig};
use persisting_dlcapt::proxy::{AppState, build_admin_router, build_public_router};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::oneshot;
use tower::ServiceExt;

type Captured = Arc<Mutex<Vec<(String, Value)>>>;

async fn start_mock_upstream(captured: Captured) -> (String, oneshot::Sender<()>) {
    let captured_chat = Arc::clone(&captured);
    let captured_responses = Arc::clone(&captured);
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured_chat = Arc::clone(&captured_chat);
                async move {
                    captured_chat
                        .lock()
                        .unwrap()
                        .push(("/v1/chat/completions".into(), body.clone()));
                    if body.get("stream").and_then(|v| v.as_bool()) == Some(true) {
                        (
                            [(header::CONTENT_TYPE, "text/event-stream")],
                            "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
                             data: [DONE]\n\n",
                        )
                            .into_response()
                    } else {
                        Json(json!({
                            "id": "chatcmpl-test",
                            "choices": [{
                                "index": 0,
                                "message": {"role":"assistant","content":"ok"},
                                "finish_reason": "stop"
                            }],
                            "usage": {"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
                        }))
                        .into_response()
                    }
                }
            }),
        )
        .route(
            "/v1/responses",
            post(move |Json(body): Json<Value>| {
                let captured_responses = Arc::clone(&captured_responses);
                async move {
                    captured_responses
                        .lock()
                        .unwrap()
                        .push(("/v1/responses".into(), body));
                    Json(json!({
                        "id": "resp-test",
                        "output": [{"content":[{"type":"output_text","text":"r-ok"}]}]
                    }))
                    .into_response()
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://{addr}/v1"), shutdown_tx)
}

fn base_config(store_dir: &str, upstream: &str) -> ProxyConfig {
    ProxyConfig {
        listen: "127.0.0.1:0".to_string(),
        admin_listen: "127.0.0.1:0".to_string(),
        store_dir: store_dir.to_string(),
        agent_id: "openclaw".to_string(),
        session_header: "x-persisting-session-id".to_string(),
        session_header_aliases: vec![],
        default_session_id: "default".to_string(),
        preserve_raw: false,
        base_session_path: "/v1/sessions".to_string(),
        storage: StorageConfig::default(),
        export: ExportConfig::default(),
        models: vec![
            ModelRoute {
                name: "kimi-k2.5".to_string(),
                display_name: Some("Kimi K2.5".to_string()),
                provider: "openai".to_string(),
                upstream_base_url: upstream.to_string(),
                api_key: Some("".to_string()),
            },
            ModelRoute {
                name: "*".to_string(),
                display_name: Some("Fallback".to_string()),
                provider: "openai".to_string(),
                upstream_base_url: upstream.to_string(),
                api_key: Some("".to_string()),
            },
        ],
    }
}

async fn body_bytes(response: axum::response::Response) -> bytes::Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

async fn body_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

async fn wait_for_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("capture artifact was not written: {}", path.display()));
}

async fn closed_loopback_upstream() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}/v1")
}

#[tokio::test]
async fn health_ready_models_admin_sessions_contract() {
    let dir = tempdir().unwrap();
    let state = AppState::new(base_config(
        dir.path().to_str().unwrap(),
        "http://127.0.0.1:9/v1",
    ));
    let public = build_public_router(state.clone());
    let admin = build_admin_router(state);

    let health = public
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(body_json(health).await["status"], "ok");

    let ready = public
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(body_json(ready).await["status"], "ready");

    let models = public
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(models.status(), StatusCode::OK);
    let models_json = body_json(models).await;
    assert!(
        models_json["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "kimi-k2.5")
    );

    let sessions = admin
        .oneshot(
            Request::builder()
                .uri("/admin/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sessions.status(), StatusCode::OK);
    assert!(body_json(sessions).await.get("sessions").is_some());
}

#[tokio::test]
async fn flat_chat_completions_forwards_to_upstream() {
    let dir = tempdir().unwrap();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let (upstream, shutdown) = start_mock_upstream(Arc::clone(&captured)).await;
    let state = AppState::new(base_config(dir.path().to_str().unwrap(), &upstream));
    let app = build_public_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "kimi-k2.5",
                        "messages": [{"role":"user","content":"ping"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured_req = captured.lock().unwrap();
    assert_eq!(captured_req[0].0, "/v1/chat/completions");
    assert_eq!(captured_req[0].1["model"], "kimi-k2.5");
    let _ = shutdown.send(());
}

#[tokio::test]
async fn session_url_overrides_header_and_body_session() {
    let dir = tempdir().unwrap();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let (upstream, shutdown) = start_mock_upstream(Arc::clone(&captured)).await;
    let state = AppState::new(base_config(dir.path().to_str().unwrap(), &upstream));
    let app = build_public_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions/session-a/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-persisting-session-id", "header-session")
                .body(Body::from(
                    json!({
                        "model": "kimi-k2.5",
                        "messages": [{"role":"user","content":"ping"}],
                        "metadata": {"session_id": "body-session"},
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    wait_for_file(&dir.path().join("session-a/trajectory.md")).await;
    wait_for_file(&dir.path().join("session-a/session_steps.json")).await;
    let _ = shutdown.send(());
}

#[tokio::test]
async fn session_responses_forwards_to_responses_path() {
    let dir = tempdir().unwrap();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let (upstream, shutdown) = start_mock_upstream(Arc::clone(&captured)).await;
    let state = AppState::new(base_config(dir.path().to_str().unwrap(), &upstream));
    let app = build_public_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions/session-a/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "kimi-k2.5",
                        "input": "ping"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let captured_req = captured.lock().unwrap();
    assert_eq!(captured_req[0].0, "/v1/responses");
    let _ = shutdown.send(());
}

#[tokio::test]
async fn streaming_chat_returns_sse_and_writes_capture() {
    let dir = tempdir().unwrap();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let (upstream, shutdown) = start_mock_upstream(Arc::clone(&captured)).await;
    let state = AppState::new(base_config(dir.path().to_str().unwrap(), &upstream));
    let app = build_public_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions/session-stream/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "kimi-k2.5",
                        "messages": [{"role":"user","content":"ping"}],
                        "stream": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("text/event-stream"), "content-type={ct}");
    let _ = body_bytes(response).await;
    wait_for_file(&dir.path().join("session-stream/trajectory.md")).await;
    wait_for_file(&dir.path().join("session-stream/session_steps.json")).await;
    let _ = shutdown.send(());
}

#[tokio::test]
async fn upstream_unreachable_returns_502_and_admin_error() {
    let dir = tempdir().unwrap();
    let upstream = closed_loopback_upstream().await;
    let state = AppState::new(base_config(dir.path().to_str().unwrap(), &upstream));
    let public = build_public_router(state.clone());
    let admin = build_admin_router(state);

    let response = public
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "kimi-k2.5",
                        "messages": [{"role":"user","content":"ping"}],
                        "stream": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);

    let errors = admin
        .oneshot(
            Request::builder()
                .uri("/admin/errors")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(errors.status(), StatusCode::OK);
    let payload = body_json(errors).await;
    let list = payload["recent_errors"].as_array().unwrap_or_else(|| {
        panic!("expected recent_errors array, got {payload}");
    });
    assert!(
        !list.is_empty(),
        "admin errors should record upstream failure: {payload}"
    );
}
