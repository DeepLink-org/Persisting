use crate::dialogue_extract::is_subagent_shape_payload;
use crate::record::CaptureRecord;

pub fn should_skip_record(rec: &CaptureRecord) -> bool {
    match rec.kind.as_str() {
        "llm.request" => {
            if rec.is_internal_llm_request() {
                return true;
            }
            if should_skip_main_flash_companion_request(rec) {
                return true;
            }
            rec.visible_user_text().is_none()
        }
        "llm.response" | "llm.response.stream" => {
            if rec.payload.get("stream_partial").and_then(|v| v.as_bool()) == Some(true) {
                return true;
            }
            rec.visible_assistant_text().is_none()
        }
        "llm.spawn_link" => false,
        "llm.call.cancelled" => true,
        k if k.starts_with("session.") => true,
        _ => false,
    }
}

pub fn should_refresh_frontmatter(rec: &CaptureRecord) -> bool {
    matches!(
        rec.kind.as_str(),
        "llm.request" | "llm.response" | "llm.response.stream"
    )
}

fn should_skip_main_flash_companion_request(rec: &CaptureRecord) -> bool {
    if rec.subagent_id.is_some() {
        return false;
    }
    let model = rec
        .payload
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("");
    if !model.contains("flash") && !model.contains("haiku") {
        return false;
    }
    if rec.visible_user_text().is_none() {
        return false;
    }
    !is_subagent_shape_payload(&rec.payload)
}
