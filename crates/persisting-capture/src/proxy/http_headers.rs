//! Shared HTTP header filtering for forward proxy and upstream auth.

use axum::http::HeaderMap;

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// RFC 7230 hop-by-hop headers (also used for response filtering).
pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// Headers to omit when transparently forwarding a client request.
pub fn skip_transparent_forward_header(name: &str) -> bool {
    is_hop_by_hop(name) || name.eq_ignore_ascii_case("host")
}

/// Headers to omit when building an upstream LLM request from client headers.
pub fn skip_upstream_forward_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "x-api-key" | "authorization" | "expect"
    ) || is_hop_by_hop(name)
}

/// Client requested HTTP → WebSocket upgrade (e.g. Codex `/v1/responses` probe).
pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_websocket_upgrade() {
        let mut h = HeaderMap::new();
        h.insert("upgrade", "websocket".parse().unwrap());
        assert!(is_websocket_upgrade(&h));
    }
}
