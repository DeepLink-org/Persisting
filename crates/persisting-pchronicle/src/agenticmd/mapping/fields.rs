use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Value};

use crate::formats::events::EventRecord;

use super::text::{compact_json, content_to_string, visible_assistant_text, visible_user_text};

pub(super) fn role_and_body(rec: &EventRecord) -> Result<(String, String)> {
    Ok(match rec.kind.as_str() {
        "llm.request" | "http.request" => {
            ("user".into(), visible_user_text(rec).unwrap_or_default())
        }
        "llm.response" | "llm.response.stream" | "http.response" | "http.response.stream" => (
            "assistant".into(),
            visible_assistant_text(rec).unwrap_or_default(),
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

pub(super) fn attach_subagent_link_fields(fields: &mut BTreeMap<String, Value>, rec: &EventRecord) {
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

pub(super) fn attach_llm_fields(fields: &mut BTreeMap<String, Value>, rec: &EventRecord) {
    match rec.kind.as_str() {
        "llm.request" | "http.request" => {
            if let Some(model) = rec.payload.get("model").and_then(|v| v.as_str()) {
                fields.insert("model".into(), json!(model));
            }
            if let Some(path) = rec.payload.get("path").and_then(|v| v.as_str()) {
                fields.insert("path".into(), json!(path));
            }
        }
        "llm.response" | "llm.response.stream" | "http.response" | "http.response.stream" => {
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
