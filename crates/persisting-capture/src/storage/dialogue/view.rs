use serde_json::Value;

use super::fields::role_and_body;
use super::skip_markdown_block;
use crate::record::CaptureRecord;

/// Queryable columns extracted from a capture record (shared by Vortex + markdown block headers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRecordView {
    pub role: Option<String>,
    pub text: Option<String>,
    pub model: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
}

/// Visible dialogue text and LLM summary fields; omits text when markdown would skip the block.
pub fn capture_record_view(rec: &CaptureRecord) -> CaptureRecordView {
    let mut view = CaptureRecordView::default();
    attach_llm_view(&mut view, rec);
    if skip_markdown_block(rec) {
        return view;
    }
    if let Ok((role, body)) = role_and_body(rec) {
        view.role = Some(role);
        if !body.is_empty() {
            view.text = Some(body);
        }
    }
    view
}

fn attach_llm_view(view: &mut CaptureRecordView, rec: &CaptureRecord) {
    match rec.kind.as_str() {
        "llm.request" => {
            if let Some(model) = rec.payload.get("model").and_then(|v| v.as_str()) {
                view.model = Some(model.to_string());
            }
            if let Some(path) = rec.payload.get("path").and_then(|v| v.as_str()) {
                view.path = Some(path.to_string());
            }
        }
        "llm.response" | "llm.response.stream" => {
            if let Some(status) = rec.payload.get("status").and_then(|v| v.as_i64()) {
                view.status = Some(status as i32);
            }
            if let Some(usage) = rec
                .payload
                .get("body")
                .and_then(|b| b.get("usage"))
                .or_else(|| rec.payload.get("usage"))
            {
                view.prompt_tokens = token_field(usage, "prompt_tokens", "input_tokens");
                view.completion_tokens = token_field(usage, "completion_tokens", "output_tokens");
                view.total_tokens = usage.get("total_tokens").and_then(|v| v.as_i64());
            }
        }
        _ => {}
    }
}

fn token_field(usage: &Value, primary: &str, alias: &str) -> Option<i64> {
    usage
        .get(primary)
        .or_else(|| usage.get(alias))
        .and_then(|v| v.as_i64())
}
