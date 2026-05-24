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

/// `CONNECT host:443` — tunnel TCP to target (HTTPS and other TLS).
pub async fn handle_connect(req: Request) -> Response {
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
    Ok(builder
        .body(Body::from(bytes))
        .map_err(|e| anyhow::anyhow!("build response: {e}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
