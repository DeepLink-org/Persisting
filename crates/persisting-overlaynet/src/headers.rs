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
    use proptest::prelude::*;

    fn token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z][a-z0-9-]{0,12}").unwrap()
    }

    fn hop_header_strategy() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just("connection"),
            Just("proxy-connection"),
            Just("keep-alive"),
            Just("proxy-authenticate"),
            Just("proxy-authorization"),
            Just("te"),
            Just("trailers"),
            Just("transfer-encoding"),
            Just("upgrade"),
        ]
    }

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

    proptest! {
        #[test]
        fn fixed_hop_headers_are_case_insensitive(name in hop_header_strategy()) {
            prop_assert!(is_hop_by_hop(name));
            prop_assert!(is_hop_by_hop(&name.to_ascii_uppercase()));
            prop_assert!(skip_transparent_forward_header(&name.to_ascii_uppercase()));
        }

        #[test]
        fn connection_tokens_nominate_headers_with_arbitrary_spacing_and_case(
            token in token_strategy(),
            uppercase in any::<bool>(),
        ) {
            let nominated = if uppercase {
                token.to_ascii_uppercase()
            } else {
                token.clone()
            };
            let mut headers = HeaderMap::new();
            headers.insert(
                "connection",
                format!(" keep-alive ,  {nominated} ").parse().unwrap(),
            );
            let non_nominated_header = format!("{}-other", token);
            prop_assert!(skip_transparent_forward_header_for(&headers, &token));
            prop_assert!(!skip_transparent_forward_header_for(
                &headers,
                &non_nominated_header,
            ));
        }

        #[test]
        fn reframing_always_removes_content_length_and_conditionally_removes_content_metadata(
            body_changed in any::<bool>(),
        ) {
            let headers = HeaderMap::new();
            prop_assert!(skip_response_header_after_reframing(
                &headers,
                "content-length",
                body_changed,
            ));
            prop_assert_eq!(
                skip_response_header_after_reframing(&headers, "content-type", body_changed),
                body_changed,
            );
            prop_assert_eq!(
                skip_response_header_after_reframing(&headers, "content-encoding", body_changed),
                body_changed,
            );
        }

        #[test]
        fn websocket_upgrade_detection_is_token_based_not_substring(
            prefix in token_strategy(),
            suffix in token_strategy(),
        ) {
            let mut headers = HeaderMap::new();
            headers.insert(
                "upgrade",
                format!("{prefix}, WebSocket, {suffix}").parse().unwrap(),
            );
            prop_assert!(is_websocket_upgrade(&headers));
            headers.insert("upgrade", format!("{prefix}websocket{suffix}").parse().unwrap());
            prop_assert!(!is_websocket_upgrade(&headers));
        }
    }
}
