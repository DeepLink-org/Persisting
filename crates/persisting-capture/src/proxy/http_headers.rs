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

/// Headers to omit when forwarding an upstream response back to the client whose body
/// has been rewritten (protocol translation, JSON re-encoding, etc.).
///
/// Body-sensitive headers (`content-length`, `content-encoding`) become incorrect
/// because the rewritten body has a different size and is no longer compressed;
/// `content-type` may also be different (translation flips JSON vs SSE). Hop-by-hop
/// headers (`transfer-encoding`, `connection`, …) are likewise unsafe to forward
/// blindly. Callers should re-set `content-type` from the rewrite and let the HTTP
/// stack recompute `content-length` from the body.
pub fn skip_response_header_when_body_changed(name: &str) -> bool {
    is_hop_by_hop(name)
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding" | "content-type"
        )
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

    #[test]
    fn body_change_filter_drops_size_encoding_and_type_headers() {
        for h in [
            "content-length",
            "Content-Length",
            "content-encoding",
            "content-type",
            "transfer-encoding",
            "connection",
        ] {
            assert!(
                skip_response_header_when_body_changed(h),
                "expected `{h}` to be filtered when body is rewritten"
            );
        }
    }

    #[test]
    fn body_change_filter_keeps_other_headers() {
        for h in ["x-request-id", "anthropic-version", "x-ratelimit-remaining"] {
            assert!(
                !skip_response_header_when_body_changed(h),
                "expected `{h}` to be forwarded"
            );
        }
    }
}
