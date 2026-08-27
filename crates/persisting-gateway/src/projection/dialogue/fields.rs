use anyhow::Result;
use serde_json::Value;

use crate::record::{EventRecord, EventRecordExt, content_to_string};

pub(crate) fn role_and_body(rec: &EventRecord) -> Result<(String, String)> {
    Ok(match rec.kind.as_str() {
        "llm.request" => ("user".into(), rec.visible_user_text().unwrap_or_default()),
        "llm.response" | "llm.response.stream" => (
            "assistant".into(),
            rec.visible_assistant_text().unwrap_or_default(),
        ),
        "user" | "assistant" | "system" | "tool" | "note" => (
            rec.kind.clone(),
            rec.payload
                .get("content")
                .and_then(content_to_string)
                .unwrap_or_else(|| compact_json(&rec.payload)),
        ),
        _ => ("note".into(), compact_json(&rec.payload)),
    })
}

pub(crate) fn compact_json(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
}
