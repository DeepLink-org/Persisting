//! HTTP forward proxy: `CONNECT` tunnel + absolute-URI transparent forward.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;

use crate::protocol::ProtocolKind;

use super::http_headers::skip_transparent_forward_header;
use super::network_policy::{
    assert_egress_allowed, forbidden_response, host_from_authority, NetworkPolicy,
};

/// `CONNECT host:443` — tunnel TCP to target (HTTPS and other TLS).
pub async fn handle_connect(req: Request, policy: &NetworkPolicy) -> Response {
    let Some(authority) = req.uri().authority().map(|a| a.to_string()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let host = host_from_authority(&authority);
    if let Err(reason) = assert_egress_allowed(policy, &host) {
        let (status, msg) = forbidden_response(&host, &reason);
        return (status, msg).into_response();
    }
    handle_connect_authorized(req).await
}

/// Execute a CONNECT request after the caller's pVisor controller authorized it.
pub async fn handle_connect_authorized(req: Request) -> Response {
    let Some(authority) = req.uri().authority().map(|a| a.to_string()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let target = connect_target(&authority);
    let on_upgrade: OnUpgrade = hyper::upgrade::on(req);
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let Ok(mut dst) = TcpStream::connect(&target).await else {
            return;
        };
        let mut client = TokioIo::new(upgraded);
        let _ = copy_bidirectional(&mut client, &mut dst).await;
    });
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .expect("CONNECT 200")
        .into_response()
}

fn connect_target(authority: &str) -> String {
    if authority.starts_with('[') {
        // `[ipv6]:port` or `[ipv6]`
        if authority.matches(':').count() >= 2 {
            if authority.ends_with(']') {
                return format!("{authority}:443");
            }
            return authority.to_string();
        }
    }
    if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:443")
    }
}

/// Forward-proxy form: absolute URI (`GET http://host/path`).
pub fn is_forward_proxy_request(method: &Method, uri: &Uri) -> bool {
    method == Method::CONNECT || uri.scheme().is_some()
}

pub fn is_llm_capture_path(path: &str) -> bool {
    ProtocolKind::from_path(path) != ProtocolKind::Unknown
}

pub async fn transparent_forward(
    client: &reqwest::Client,
    req: Request,
    policy: &NetworkPolicy,
) -> anyhow::Result<Response<Body>> {
    let host = req.uri().host().map(str::to_string).unwrap_or_default();
    if let Err(reason) = assert_egress_allowed(policy, &host) {
        let (status, msg) = forbidden_response(&host, &reason);
        return Ok(Response::builder()
            .status(status)
            .body(Body::from(msg))
            .expect("403 body"));
    }
    transparent_forward_authorized(client, req).await
}

/// Forward an absolute-URI request after the caller's pVisor controller
/// authorized it.
pub async fn transparent_forward_authorized(
    client: &reqwest::Client,
    req: Request,
) -> anyhow::Result<Response<Body>> {
    let (parts, body) = req.into_parts();
    let url = parts.uri.to_string();
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| anyhow::anyhow!("read body: {e}"))?
        .to_bytes();

    let mut rb = client.request(parts.method, url);
    for (name, value) in parts.headers.iter() {
        let n = name.as_str();
        if skip_transparent_forward_header(n) {
            continue;
        }
        rb = rb.header(name, value);
    }
    rb = rb.body(body_bytes.to_vec());

    let resp = rb
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("forward request: {e}"))?;
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.bytes().await?;

    let mut builder = Response::builder().status(status);
    for (name, value) in headers.iter() {
        let n = name.as_str();
        if skip_transparent_forward_header(n) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(bytes))
        .map_err(|e| anyhow::anyhow!("build response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CaptureLevel, ModelRoute, NetworkConfig, NetworkMode, ProxyConfig};
    use crate::proxy::network_policy::NetworkPolicy;

    fn policy(mode: NetworkMode, allowed: &[&str]) -> NetworkPolicy {
        NetworkPolicy::from_config(&ProxyConfig {
            listen: "127.0.0.1:19081".into(),
            admin_listen: "127.0.0.1:9876".into(),
            agent_id: "default".into(),
            session_header: "x-persisting-session-id".into(),
            capture_level: CaptureLevel::Dialogue,
            debug: false,
            network: NetworkConfig {
                mode,
                allowed_hosts: allowed.iter().map(|s| (*s).to_string()).collect(),
            },
            overlay: Default::default(),
            models: vec![ModelRoute {
                name: "*".into(),
                provider: None,
                upstream: Some("http://127.0.0.1:9/v1".into()),
                upstream_anthropic: None,
                path_prefix: None,
                api_key_env: None,
                api_key: None,
                forward: None,
            }],
        })
        .unwrap()
    }

    #[test]
    fn forward_proxy_detection() {
        let u: Uri = "http://example.com/foo".parse().unwrap();
        assert!(is_forward_proxy_request(&Method::GET, &u));
        let u: Uri = "/v1/chat/completions".parse().unwrap();
        assert!(!is_forward_proxy_request(&Method::POST, &u));
    }

    #[test]
    fn llm_paths() {
        assert!(is_llm_capture_path("/v1/chat/completions"));
        assert!(!is_llm_capture_path("/pypi/simple/"));
    }

    #[test]
    fn connect_target_defaults_port() {
        assert_eq!(connect_target("api.openai.com"), "api.openai.com:443");
        assert_eq!(connect_target("127.0.0.1:8080"), "127.0.0.1:8080");
        assert_eq!(connect_target("[::1]"), "[::1]:443");
    }

    #[tokio::test]
    async fn connect_denied_before_tunnel() {
        let p = policy(NetworkMode::Allowlist, &["pypi.org"]);
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("github.com:443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn connect_allowlist_permits_listed_host() {
        let p = policy(NetworkMode::Allowlist, &["api.openai.com"]);
        // No upgrade header → tunnel task fails quietly, but status must be 200 (allowed).
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("api.openai.com:443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connect_no_network_denies() {
        let p = policy(NetworkMode::NoNetwork, &[]);
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("example.com:443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn transparent_forward_denied() {
        let p = policy(NetworkMode::Allowlist, &["pypi.org"]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri("http://github.com/")
            .body(Body::empty())
            .unwrap();
        let resp = transparent_forward(&client, req, &p).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn public_connect_allows() {
        let p = policy(NetworkMode::Public, &[]);
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("anywhere.example:443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connect_loopback_allowed_under_no_network() {
        let p = policy(NetworkMode::NoNetwork, &[]);
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("127.0.0.1:9443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn transparent_forward_allows_listed_local() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let app = axum::Router::new().route("/", axum::routing::get(|| async { "pong" }));
            axum::serve(listener, app).await.ok();
        });
        // Brief yield so accept loop starts.
        tokio::task::yield_now().await;

        let p = policy(NetworkMode::Allowlist, &["127.0.0.1"]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!("http://127.0.0.1:{}/", addr.port()))
            .body(Body::empty())
            .unwrap();
        let resp = transparent_forward(&client, req, &p).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connect_deny_body_explains_reason() {
        let p = policy(NetworkMode::Allowlist, &["pypi.org"]);
        let req = Request::builder()
            .method(Method::CONNECT)
            .uri("github.com:443")
            .body(Body::empty())
            .unwrap();
        let resp = handle_connect(req, &p).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let bytes = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap()
            .to_bytes();
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("github.com"));
        assert!(body.contains("not-in-allowlist") || body.contains("denied"));
    }
}
