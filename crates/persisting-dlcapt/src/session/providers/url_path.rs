pub fn extract_url_path_session(raw_path_session_id: &str) -> Option<String> {
    let trimmed = raw_path_session_id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
