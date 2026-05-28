//! Extract subagent ids and spawn hints from Claude traffic.

use std::collections::HashSet;

use lazy_static::lazy_static;
use regex::Regex;
use serde_json::Value;

use super::types::SpawnHint;
use crate::record::CaptureRecord;

lazy_static! {
    static ref AGENT_ID_JSON: Regex =
        Regex::new(r#"(?i)"agentId"\s*:\s*"([a-zA-Z0-9_-]{4,})""#).expect("agentId json");
    /// Claude Code tool_result line: `agentId: a583b6c00c3d06436` (not CLI `--agent-id`).
    static ref AGENT_ID_TOOL_RESULT: Regex =
        Regex::new(r"(?m)(?:^|\n)\s*agentId:\s*([a-zA-Z0-9_-]{4,})").expect("agentId tool result");
    static ref TOOL_AGENT_BLOCK: Regex =
        Regex::new(r"```tool:(?:Agent|Task)\s*\n([\s\S]*?)```").expect("tool agent block");
    static ref DOC_TARGET: Regex =
        Regex::new(r"docs/src/design/[^\s\)\]]+\.zh\.md").expect("doc target path");
    static ref TASK_ID_XML: Regex =
        Regex::new(r"(?i)<task-id>\s*([a-zA-Z0-9_-]{4,})\s*</task-id>").expect("task-id xml");
}

pub(crate) fn extract_doc_target(text: &str) -> Option<String> {
    DOC_TARGET.find(text).map(|m| m.as_str().to_string())
}

pub fn extract_subagent_ids_from_request(body: &Value) -> Vec<String> {
    let mut ids = HashSet::new();
    let Some(messages) = body.get("messages").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    for msg in messages {
        let Some(content) = msg.get("content") else {
            continue;
        };
        match content {
            Value::Array(parts) => {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        if let Some(inner) = part.get("content") {
                            collect_subagent_ids_from_value(inner, &mut ids);
                        }
                    } else if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        for id in extract_subagent_ids_from_text(text) {
                            ids.insert(id);
                        }
                    } else {
                        collect_subagent_ids_from_value(part, &mut ids);
                    }
                }
            }
            other => collect_subagent_ids_from_value(other, &mut ids),
        }
    }
    let mut out: Vec<_> = ids.into_iter().collect();
    out.sort();
    out
}

fn collect_subagent_ids_from_value(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::String(s) => {
            for id in extract_subagent_ids_from_text(s) {
                out.insert(id);
            }
        }
        Value::Array(parts) => {
            for p in parts {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    for id in extract_subagent_ids_from_text(t) {
                        out.insert(id);
                    }
                }
            }
        }
        Value::Object(map) => {
            if let Some(id) = map
                .get("agentId")
                .or_else(|| map.get("agent_id"))
                .and_then(|x| x.as_str())
                .filter(|s| plausible_agent_id(s))
            {
                out.insert(id.to_string());
            }
            for v in map.values() {
                collect_subagent_ids_from_value(v, out);
            }
        }
        _ => {}
    }
}

pub fn extract_subagent_ids_from_text(text: &str) -> Vec<String> {
    let mut ids = HashSet::new();
    for cap in AGENT_ID_JSON.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str()) {
            if plausible_agent_id(id) {
                ids.insert(id.to_string());
            }
        }
    }
    for cap in AGENT_ID_TOOL_RESULT.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str()) {
            if plausible_agent_id(id) {
                ids.insert(id.to_string());
            }
        }
    }
    for cap in TASK_ID_XML.captures_iter(text) {
        if let Some(id) = cap.get(1).map(|m| m.as_str()) {
            if plausible_agent_id(id) {
                ids.insert(id.to_string());
            }
        }
    }
    let mut out: Vec<_> = ids.into_iter().collect();
    out.sort();
    out
}

pub fn extract_spawn_hints_from_assistant(text: &str) -> Vec<SpawnHint> {
    let mut hints = Vec::new();
    for cap in TOOL_AGENT_BLOCK.captures_iter(text) {
        let Some(json_str) = cap.get(1).map(|m| m.as_str()) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(json_str.trim()) else {
            continue;
        };
        let subagent_type = v
            .get("subagent_type")
            .or_else(|| v.get("agent_type"))
            .or_else(|| v.get("description"))
            .and_then(|x| x.as_str())
            .unwrap_or("Agent");
        let description = v
            .get("description")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let prompt = v.get("prompt").and_then(|x| x.as_str()).map(str::to_string);
        let doc_target = prompt.as_deref().and_then(extract_doc_target);
        hints.push(SpawnHint {
            subagent_type: subagent_type.to_string(),
            description,
            prompt,
            doc_target,
        });
    }
    hints
}

fn plausible_agent_id(id: &str) -> bool {
    id.len() >= 4
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Append visible subagent ref footer for markdown trajectory readers.
pub fn append_subagent_refs_footer(body: &str, rec: &CaptureRecord) -> String {
    let mut parts = vec![body.to_string()];
    if let Some(traj) = rec
        .payload
        .get("subagent_trajectory")
        .and_then(|v| v.as_str())
    {
        parts.push(format!("<!-- persisting:subagent-self {traj} -->"));
    }
    if let Some(paths) = rec
        .payload
        .get("subagent_trajectories")
        .and_then(|v| v.as_array())
    {
        let refs: Vec<_> = paths.iter().filter_map(|p| p.as_str()).collect();
        if !refs.is_empty() {
            parts.push(format!(
                "<!-- persisting:subagent-refs {} -->",
                refs.join(" ")
            ));
        }
    }
    if parts.len() == 1 {
        return body.to_string();
    }
    format!("{}\n", parts.join("\n"))
}
