use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Value};

use crate::record::{content_to_string, CaptureRecord};

pub(crate) fn role_and_body(rec: &CaptureRecord) -> Result<(String, String)> {
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

pub(crate) fn attach_subagent_link_fields(
    fields: &mut BTreeMap<String, Value>,
    rec: &CaptureRecord,
) {
    if let Some(id) = &rec.subagent_id {
        fields.insert("subagent_id".into(), json!(id));
    }
    if let Some(id) = &rec.parent_agent_id {
        fields.insert("parent_agent_id".into(), json!(id));
    }
    for key in [
        "refs_subagent_ids",
        "subagent_trajectories",
        "subagent_trajectory",
        "spawn_hints",
        "spawn_links",
        "parent_agent_id",
    ] {
        if let Some(v) = rec.payload.get(key) {
            fields.insert(key.into(), v.clone());
        }
    }
}

pub(crate) fn attach_llm_fields(fields: &mut BTreeMap<String, Value>, rec: &CaptureRecord) {
    match rec.kind.as_str() {
        "llm.request" => {
            if let Some(model) = rec.payload.get("model").and_then(|v| v.as_str()) {
                fields.insert("model".into(), json!(model));
            }
            if let Some(path) = rec.payload.get("path").and_then(|v| v.as_str()) {
                fields.insert("path".into(), json!(path));
            }
        }
        "llm.response" | "llm.response.stream" => {
            if let Some(status) = rec.payload.get("status") {
                fields.insert("status".into(), status.clone());
            }
            if let Some(usage) = rec
                .payload
                .get("body")
                .and_then(|b| b.get("usage"))
                .or_else(|| rec.payload.get("usage"))
            {
                for key in [
                    "prompt_tokens",
                    "completion_tokens",
                    "total_tokens",
                    "input_tokens",
                    "output_tokens",
                ] {
                    if let Some(v) = usage.get(key) {
                        fields.insert(key.into(), v.clone());
                    }
                }
                if !fields.contains_key("prompt_tokens") {
                    if let Some(v) = usage.get("input_tokens") {
                        fields.insert("prompt_tokens".into(), v.clone());
                    }
                }
                if !fields.contains_key("completion_tokens") {
                    if let Some(v) = usage.get("output_tokens") {
                        fields.insert("completion_tokens".into(), v.clone());
                    }
                }
            }
            if let Some(v) = rec.payload.get("ttft_ms") {
                fields.insert("ttft_ms".into(), v.clone());
            }
        }
        _ => {}
    }
}

pub(crate) fn compact_json(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string())
}
