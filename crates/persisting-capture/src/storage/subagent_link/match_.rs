//! Hint/link matching and payload merge helpers.

use std::collections::{HashMap, HashSet};

use bytes::Bytes;
use serde_json::{json, Value};

use crate::dialogue_extract::extract_user_message_from_request_body;
use crate::record::CaptureRecord;

use super::types::{AgentEntry, SpawnHint, SpawnLink};

pub(crate) fn first_user_message_from_json(body: &Value) -> Option<String> {
    let bytes = Bytes::from(serde_json::to_vec(body).ok()?);
    extract_user_message_from_request_body(&bytes)
}

pub(crate) fn find_agent_for_spawn<'a>(
    agents: &'a HashMap<String, AgentEntry>,
    hint: &SpawnHint,
    used: &HashSet<String>,
) -> Option<&'a AgentEntry> {
    if let Some(doc) = hint.doc_target.as_deref() {
        if let Some(entry) = agents
            .values()
            .find(|e| !used.contains(&e.agent_id) && e.doc_target.as_deref() == Some(doc))
        {
            return Some(entry);
        }
    }
    if let Some(prompt) = hint.prompt.as_deref() {
        if let Some(entry) = agents.values().find(|e| {
            !used.contains(&e.agent_id)
                && e.first_prompt
                    .as_deref()
                    .is_some_and(|p| prompts_match(p, prompt))
        }) {
            return Some(entry);
        }
    }
    None
}

fn prompts_match(a: &str, b: &str) -> bool {
    let na = normalize_prompt(a);
    let nb = normalize_prompt(b);
    if na.len() < 40 || nb.len() < 40 {
        return false;
    }
    let prefix_len = 80.min(na.len()).min(nb.len());
    na[..prefix_len] == nb[..prefix_len]
}

fn normalize_prompt(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

pub(crate) fn spawn_hint_to_json(h: &SpawnHint) -> Value {
    let mut obj = json!({ "subagent_type": h.subagent_type });
    if let Some(d) = &h.description {
        obj["description"] = json!(d);
    }
    if let Some(p) = &h.prompt {
        obj["prompt"] = json!(p);
    }
    if let Some(d) = &h.doc_target {
        obj["doc_target"] = json!(d);
    }
    obj
}

pub(crate) fn spawn_link_to_json(l: &SpawnLink) -> Value {
    let mut obj = json!({
        "subagent_type": l.subagent_type,
        "subagent_id": l.subagent_id,
        "subagent_trajectory": l.subagent_trajectory,
    });
    if let Some(d) = &l.description {
        obj["description"] = json!(d);
    }
    if let Some(d) = &l.doc_target {
        obj["doc_target"] = json!(d);
    }
    obj
}

pub(crate) fn spawn_hint_matches_link(hint: &SpawnHint, link: &SpawnLink) -> bool {
    if hint.doc_target.is_some() && hint.doc_target == link.doc_target {
        return true;
    }
    hint.description.is_some() && hint.description == link.description
}

fn same_spawn_hint(a: &SpawnHint, b: &SpawnHint) -> bool {
    if a.doc_target.is_some() && a.doc_target == b.doc_target {
        return true;
    }
    a.description.is_some() && a.description == b.description
}

pub(crate) fn merge_spawn_hints(into: &mut Vec<SpawnHint>, new_hints: &[SpawnHint]) {
    for hint in new_hints {
        if !into.iter().any(|h| same_spawn_hint(h, hint)) {
            into.push(hint.clone());
        }
    }
}

pub(crate) fn apply_spawn_links(rec: &mut CaptureRecord, links: &[SpawnLink]) {
    if links.is_empty() {
        return;
    }
    rec.payload["spawn_links"] = json!(links.iter().map(spawn_link_to_json).collect::<Vec<_>>());
    let ids: Vec<_> = links.iter().map(|l| l.subagent_id.clone()).collect();
    let paths: Vec<_> = links
        .iter()
        .map(|l| l.subagent_trajectory.clone())
        .collect();
    merge_string_array_payload(rec, "refs_subagent_ids", &ids);
    merge_string_array_payload(rec, "subagent_trajectories", &paths);
}

pub(crate) fn merge_string_array_payload(rec: &mut CaptureRecord, key: &str, values: &[String]) {
    let mut set = HashSet::new();
    if let Some(existing) = rec.payload.get(key).and_then(|v| v.as_array()) {
        for v in existing {
            if let Some(s) = v.as_str() {
                set.insert(s.to_string());
            }
        }
    }
    for v in values {
        set.insert(v.clone());
    }
    if !set.is_empty() {
        let mut merged: Vec<_> = set.into_iter().collect();
        merged.sort();
        rec.payload[key] = json!(merged);
    }
}
