//! Shared HTTP header filtering for forward proxy and upstream auth.

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
