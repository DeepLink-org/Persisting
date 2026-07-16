use axum::http::HeaderMap;

const SESSION_HEADER_ALIASES: &[&str] = &[
    "x-persisting-session-id",
    "x-session-id",
    "x-openclaw-session-id",
];

pub fn extract_header_session(
    headers: &HeaderMap,
    primary_header: &str,
    extra_aliases: &[String],
) -> Option<String> {
    let mut candidates = Vec::with_capacity(SESSION_HEADER_ALIASES.len() + extra_aliases.len() + 1);
    let primary = primary_header.trim().to_ascii_lowercase();
    if !primary.is_empty() {
        candidates.push(primary);
    }
    for alias in SESSION_HEADER_ALIASES {
        if !candidates.iter().any(|name| name == alias) {
            candidates.push((*alias).to_string());
        }
    }
    for alias in extra_aliases {
        let normalized = alias.trim().to_ascii_lowercase();
        if !normalized.is_empty() && !candidates.iter().any(|name| name == &normalized) {
            candidates.push(normalized);
        }
    }

    for name in candidates {
        if let Some(value) = headers
            .get(name.as_str())
            .and_then(|header| header.to_str().ok())
        {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}
