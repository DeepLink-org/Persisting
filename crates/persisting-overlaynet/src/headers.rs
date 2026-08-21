//! Shared HTTP header filtering for proxy forwarding.

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

pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.iter().any(|h| name.eq_ignore_ascii_case(h))
}

pub fn skip_transparent_forward_header(name: &str) -> bool {
    is_hop_by_hop(name) || name.eq_ignore_ascii_case("host")
}

/// RFC 9110 allows `Connection` to nominate additional hop-by-hop headers.
/// Those fields must be removed alongside the fixed proxy header set.
pub fn skip_transparent_forward_header_for(headers: &HeaderMap, name: &str) -> bool {
    skip_transparent_forward_header(name) || connection_nominates_header(headers, name)
}

pub fn skip_upstream_forward_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "x-api-key" | "x-goog-api-key" | "authorization" | "expect"
    ) || is_hop_by_hop(name)
}

pub fn skip_response_header_when_body_changed(name: &str) -> bool {
    is_hop_by_hop(name)
        || matches!(
            name.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding" | "content-type"
        )
}

/// Filter upstream response headers after a proxy has consumed and rebuilt the
/// response body. HTTP framing is always local to the current hop, so stale
/// transfer metadata must not survive even when the payload bytes are unchanged.
pub fn skip_response_header_after_reframing(
    headers: &HeaderMap,
    name: &str,
    body_changed: bool,
) -> bool {
    is_hop_by_hop(name)
        || connection_nominates_header(headers, name)
        || name.eq_ignore_ascii_case("content-length")
        || (body_changed
            && matches!(
                name.to_ascii_lowercase().as_str(),
                "content-encoding" | "content-type"
            ))
}

fn connection_nominates_header(headers: &HeaderMap, name: &str) -> bool {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|token| token.eq_ignore_ascii_case(name))
}

pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get_all("upgrade")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|protocol| protocol.eq_ignore_ascii_case("websocket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_hop_by_hop_and_rewritten_body_headers() {
        assert!(skip_transparent_forward_header("host"));
        assert!(skip_upstream_forward_header("authorization"));
        assert!(skip_response_header_when_body_changed("content-length"));
        assert!(!skip_response_header_when_body_changed("x-request-id"));
    }

    #[test]
    fn reframed_responses_drop_upstream_framing_and_connection_extensions() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive, x-upstream-hop".parse().unwrap());

        assert!(skip_response_header_after_reframing(
            &headers,
            "transfer-encoding",
            false
        ));
        assert!(skip_response_header_after_reframing(
            &headers,
            "content-length",
            false
        ));
        assert!(skip_response_header_after_reframing(
            &headers,
            "x-upstream-hop",
            false
        ));
        assert!(!skip_response_header_after_reframing(
            &headers,
            "content-type",
            false
        ));
        assert!(skip_response_header_after_reframing(
            &headers,
            "content-type",
            true
        ));
    }

    #[test]
    fn connection_header_can_nominate_extra_hop_by_hop_fields() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", "keep-alive, x-private-hop".parse().unwrap());
        headers.insert("x-private-hop", "secret".parse().unwrap());

        assert!(skip_transparent_forward_header_for(
            &headers,
            "x-private-hop"
        ));
        assert!(!skip_transparent_forward_header_for(
            &headers,
            "x-end-to-end"
        ));
    }

    #[test]
    fn every_fixed_hop_header_is_filtered_case_insensitively() {
        for name in HOP_BY_HOP {
            assert!(is_hop_by_hop(name));
            assert!(is_hop_by_hop(&name.to_ascii_uppercase()));
            assert!(skip_transparent_forward_header(name));
        }
        assert!(!is_hop_by_hop("x-request-id"));
    }

    #[test]
    fn multiple_connection_values_nominate_trimmed_tokens() {
        let mut headers = HeaderMap::new();
        headers.append("connection", "keep-alive".parse().unwrap());
        headers.append("connection", " x-first , X-Second ".parse().unwrap());
        assert!(skip_transparent_forward_header_for(&headers, "x-first"));
        assert!(skip_transparent_forward_header_for(&headers, "x-second"));
        assert!(!skip_transparent_forward_header_for(&headers, "x-third"));
    }

    #[test]
    fn websocket_upgrade_is_a_case_insensitive_protocol_token() {
        let mut headers = HeaderMap::new();
        headers.insert("upgrade", "h2c, WebSocket".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
        headers.insert("upgrade", "h2c".parse().unwrap());
        assert!(!is_websocket_upgrade(&headers));
    }
}
