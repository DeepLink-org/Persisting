pub fn default_session_candidate(default_session_id: &str) -> Option<String> {
    let trimmed = default_session_id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
