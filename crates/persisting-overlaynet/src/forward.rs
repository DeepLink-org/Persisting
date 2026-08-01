//! Explicit HTTP proxy forwarding: CONNECT tunnel and absolute-URI requests.

use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;

use crate::headers::skip_transparent_forward_header;
pub(crate) async fn handle_connect_authorized(req: Request) -> Response {
    let Some(authority) = req.uri().authority().map(|authority| authority.to_string()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let target = connect_target(&authority);
    let on_upgrade: OnUpgrade = hyper::upgrade::on(req);
    tokio::spawn(async move {
        let Ok(upgraded) = on_upgrade.await else {
            return;
        };
        let Ok(mut destination) = TcpStream::connect(&target).await else {
            return;
        };
        let mut client = TokioIo::new(upgraded);
        let _ = copy_bidirectional(&mut client, &mut destination).await;
    });
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::empty())
        .expect("CONNECT response")
        .into_response()
}

fn connect_target(authority: &str) -> String {
    if authority.starts_with('[') && authority.matches(':').count() >= 2 {
        if authority.ends_with(']') {
            return format!("{authority}:443");
        }
        return authority.to_string();
    }
    if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{authority}:443")
    }
}

pub fn is_forward_proxy_request(method: &Method, uri: &Uri) -> bool {
    method == Method::CONNECT || uri.scheme().is_some()
}

pub(crate) async fn transparent_forward_authorized(
    client: &reqwest::Client,
    req: Request,
) -> anyhow::Result<Response<Body>> {
    let (parts, body) = req.into_parts();
    let url = parts.uri.to_string();
    let body_bytes = body
        .collect()
        .await
        .map_err(|error| anyhow::anyhow!("read body: {error}"))?
        .to_bytes();

    let mut request = client.request(parts.method, url);
    for (name, value) in &parts.headers {
        if !skip_transparent_forward_header(name.as_str()) {
            request = request.header(name, value);
        }
    }
    let response = request
        .body(body_bytes.to_vec())
        .send()
        .await
        .map_err(|error| anyhow::anyhow!("forward request: {error}"))?;
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.bytes().await?;

    let mut builder = Response::builder().status(status);
    for (name, value) in &headers {
        if !skip_transparent_forward_header(name.as_str()) {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(Body::from(bytes))
        .map_err(|error| anyhow::anyhow!("build response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_explicit_proxy_requests() {
        let uri: Uri = "http://example.com/path".parse().unwrap();
        assert!(is_forward_proxy_request(&Method::GET, &uri));
        assert!(!is_forward_proxy_request(
            &Method::GET,
            &"/v1/messages".parse().unwrap()
        ));
    }

    #[test]
    fn connect_target_defaults_to_https_port() {
        assert_eq!(connect_target("example.com"), "example.com:443");
        assert_eq!(connect_target("[::1]"), "[::1]:443");
        assert_eq!(connect_target("example.com:8443"), "example.com:8443");
    }
}
