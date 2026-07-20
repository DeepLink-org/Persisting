use serde_json::Value;

pub fn extract_body_metadata_session(body: &Value) -> Option<String> {
    let session_id = body
        .get("metadata")
        .and_then(|meta| meta.get("session_id"))
        .and_then(Value::as_str)?;
    let trimmed = session_id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
