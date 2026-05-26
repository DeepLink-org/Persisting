//! Main agent ↔ subagent cross-reference for trajectory capture.

use std::collections::{HashMap, HashSet};

use axum::http::HeaderMap;
use bytes::Bytes;
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::{json, Value};

use super::dialogue_extract::extract_user_message_from_request_body;
use super::record::CaptureRecord;
use super::session::CaptureRoute;
use crate::session_chain::extract_claude_parent_agent_id;

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

#[derive(Debug, Clone, Default)]
pub struct AgentEntry {
    pub agent_id: String,
    pub storage_leaf: String,
    pub first_call_id: Option<String>,
    pub first_prompt: Option<String>,
    pub doc_target: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct PendingMainSpawn {
    parent_call_id: String,
    hints: Vec<SpawnHint>,
    links: Vec<SpawnLink>,
    matched_agents: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
struct RunSubagents {
    agents: HashMap<String, AgentEntry>,
    pending_mains: Vec<PendingMainSpawn>,
    main_storage_session_id: Option<String>,
}

/// Tracks subagent instances seen during one `capture run` root session.
#[derive(Debug, Default)]
pub struct SubagentRegistry {
    runs: HashMap<String, RunSubagents>,
}

impl SubagentRegistry {
    pub fn tracked_run_count(&self) -> usize {
        self.runs.len()
    }

    fn run_mut(&mut self, run_key: &str) -> &mut RunSubagents {
        self.runs.entry(run_key.to_string()).or_default()
    }

    pub fn register_route(
        &mut self,
        run_key: &str,
        route: &CaptureRoute,
        call_id: &str,
        request_body: Option<&Value>,
    ) {
        let run = self.run_mut(run_key);
        if route.subagent_id.is_none() {
            run.main_storage_session_id = Some(route.storage_session_id.clone());
        }
        let Some(id) = route.subagent_id.as_deref() else {
            return;
        };
        let prompt = request_body.and_then(first_user_message_from_json);
        let doc_target = prompt.as_deref().and_then(extract_doc_target);
        run.agents
            .entry(id.to_string())
            .and_modify(|e| {
                if e.first_prompt.is_none() {
                    e.first_prompt = prompt.clone();
                }
                if e.doc_target.is_none() {
                    e.doc_target = doc_target.clone();
                }
            })
            .or_insert_with(|| AgentEntry {
                agent_id: id.to_string(),
                storage_leaf: route.storage_session_id.clone(),
                first_call_id: Some(call_id.to_string()),
                first_prompt: prompt,
                doc_target,
            });
    }

    pub fn trajectory_paths(&self, run_key: &str, ids: &[String]) -> Vec<String> {
        let Some(run) = self.runs.get(run_key) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| {
                run.agents
                    .get(id)
                    .map(|e| subagent_trajectory_path(&e.storage_leaf))
            })
            .collect()
    }

    /// Match ```tool:Agent``` spawn hints to subagents already registered in this run.
    pub fn match_spawns_to_agents(&self, run_key: &str, hints: &[SpawnHint]) -> Vec<SpawnLink> {
        let Some(run) = self.runs.get(run_key) else {
            return Vec::new();
        };
        let mut used = HashSet::new();
        let mut links = Vec::new();
        for hint in hints {
            let Some(agent) = find_agent_for_spawn(&run.agents, hint, &used) else {
                continue;
            };
            used.insert(agent.agent_id.clone());
            links.push(SpawnLink {
                subagent_type: hint.subagent_type.clone(),
                description: hint.description.clone(),
                doc_target: hint.doc_target.clone(),
                subagent_id: agent.agent_id.clone(),
                subagent_trajectory: subagent_trajectory_path(&agent.storage_leaf),
            });
        }
        links
    }

    fn remember_main_spawn(
        &mut self,
        run_key: &str,
        parent_call_id: &str,
        hints: &[SpawnHint],
    ) -> Vec<SpawnLink> {
        if hints.is_empty() {
            return Vec::new();
        }
        let merged = {
            let run = self.run_mut(run_key);
            if let Some(existing) = run
                .pending_mains
                .iter_mut()
                .find(|p| p.parent_call_id == parent_call_id)
            {
                merge_spawn_hints(&mut existing.hints, hints);
                existing.hints.clone()
            } else {
                run.pending_mains.push(PendingMainSpawn {
                    parent_call_id: parent_call_id.to_string(),
                    hints: hints.to_vec(),
                    links: Vec::new(),
                    matched_agents: HashSet::new(),
                });
                hints.to_vec()
            }
        };
        let links = self.match_spawns_to_agents(run_key, &merged);
        let run = self.run_mut(run_key);
        if let Some(existing) = run
            .pending_mains
            .iter_mut()
            .find(|p| p.parent_call_id == parent_call_id)
        {
            existing.hints = merged;
            existing.links = links.clone();
            existing.matched_agents = links.iter().map(|l| l.subagent_id.clone()).collect();
        }
        links
    }

    fn on_subagent_registered(&mut self, run_key: &str, agent_id: &str) -> Vec<SpawnLinkBackfill> {
        let Some(agent) = self
            .runs
            .get(run_key)
            .and_then(|r| r.agents.get(agent_id).cloned())
        else {
            return Vec::new();
        };
        let run = self.run_mut(run_key);
        let mut backfills = Vec::new();
        for pending in &mut run.pending_mains {
            if pending.matched_agents.contains(agent_id) {
                continue;
            }
            let mut used = pending.matched_agents.clone();
            let mut new_links = Vec::new();
            for hint in &pending.hints {
                if pending
                    .links
                    .iter()
                    .any(|l| spawn_hint_matches_link(hint, l))
                {
                    continue;
                }
                let single = HashMap::from([(agent.agent_id.clone(), agent.clone())]);
                if find_agent_for_spawn(&single, hint, &used).is_some() {
                    let link = SpawnLink {
                        subagent_type: hint.subagent_type.clone(),
                        description: hint.description.clone(),
                        doc_target: hint.doc_target.clone(),
                        subagent_id: agent.agent_id.clone(),
                        subagent_trajectory: subagent_trajectory_path(&agent.storage_leaf),
                    };
                    used.insert(agent.agent_id.clone());
                    pending.links.push(link.clone());
                    new_links.push(link);
                }
            }
            if !new_links.is_empty() {
                pending.matched_agents = used;
                backfills.push(SpawnLinkBackfill {
                    parent_call_id: pending.parent_call_id.clone(),
                    links: new_links,
                });
            }
        }
        backfills
    }
}

fn first_user_message_from_json(body: &Value) -> Option<String> {
    let bytes = Bytes::from(serde_json::to_vec(body).ok()?);
    extract_user_message_from_request_body(&bytes)
}

fn extract_doc_target(text: &str) -> Option<String> {
    DOC_TARGET.find(text).map(|m| m.as_str().to_string())
}

fn find_agent_for_spawn<'a>(
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

fn spawn_hint_to_json(h: &SpawnHint) -> Value {
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

fn spawn_link_to_json(l: &SpawnLink) -> Value {
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

fn spawn_hint_matches_link(hint: &SpawnHint, link: &SpawnLink) -> bool {
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

fn merge_spawn_hints(into: &mut Vec<SpawnHint>, new_hints: &[SpawnHint]) {
    for hint in new_hints {
        if !into.iter().any(|h| same_spawn_hint(h, hint)) {
            into.push(hint.clone());
        }
    }
}

fn apply_spawn_links(rec: &mut CaptureRecord, links: &[SpawnLink]) {
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

pub fn spawn_link_backfill_record(
    parent_call_id: &str,
    links: &[SpawnLink],
    call: &crate::capture_call::CaptureCall,
) -> CaptureRecord {
    let mut payload = serde_json::json!({
        "parent_call_id": parent_call_id,
        "spawn_links": links.iter().map(spawn_link_to_json).collect::<Vec<_>>(),
    });
    let ids: Vec<_> = links.iter().map(|l| l.subagent_id.clone()).collect();
    let paths: Vec<_> = links
        .iter()
        .map(|l| l.subagent_trajectory.clone())
        .collect();
    payload["refs_subagent_ids"] = json!(ids);
    payload["subagent_trajectories"] = json!(paths);
    let rec = CaptureRecord {
        seq: 0,
        source: "persisting-proxy".to_string(),
        kind: "llm.spawn_link".to_string(),
        timestamp: None,
        session_id: None,
        agent_id: None,
        parent_uuid: None,
        trace_id: Some(call.trace_id.clone()),
        call_id: Some(parent_call_id.to_string()),
        subagent_id: None,
        parent_agent_id: None,
        branch: None,
        parent_call_id: Some(parent_call_id.to_string()),
        payload,
    };
    rec
}

pub fn subagent_trajectory_path(storage_leaf: &str) -> String {
    format!("{storage_leaf}.md")
}

pub fn main_route_for_backfill(route: &CaptureRoute, registry: &SubagentRegistry) -> CaptureRoute {
    let run_key = run_registry_key(route);
    let storage_session_id = registry
        .runs
        .get(&run_key)
        .and_then(|r| r.main_storage_session_id.clone())
        .or_else(|| route.root_session.clone())
        .unwrap_or_else(|| route.storage_session_id.clone());
    CaptureRoute {
        root_session: route.root_session.clone(),
        session_id: route.session_id.clone(),
        storage_session_id,
        subagent_id: None,
    }
}

pub fn run_registry_key(route: &CaptureRoute) -> String {
    route
        .root_session
        .clone()
        .unwrap_or_else(|| route.storage_session_id.clone())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnHint {
    pub subagent_type: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub doc_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnLinkBackfill {
    pub parent_call_id: String,
    pub links: Vec<SpawnLink>,
}

/// Side effects from [`enrich_record`] — append backfill records on the main session route.
#[derive(Debug, Clone, Default)]
pub struct EnrichOutcome {
    pub spawn_link_backfills: Vec<SpawnLinkBackfill>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnLink {
    pub subagent_type: String,
    pub description: Option<String>,
    pub doc_target: Option<String>,
    pub subagent_id: String,
    pub subagent_trajectory: String,
}

/// Attach cross-reference fields to a capture record (metadata + payload).
pub fn enrich_record(
    rec: &mut CaptureRecord,
    route: &CaptureRoute,
    headers: &HeaderMap,
    request_body: Option<&Value>,
    assistant_text: Option<&str>,
    registry: &mut SubagentRegistry,
) -> EnrichOutcome {
    let mut outcome = EnrichOutcome::default();
    let run_key = run_registry_key(route);
    registry.register_route(
        &run_key,
        route,
        rec.call_id.as_deref().unwrap_or(""),
        request_body,
    );

    if let Some(id) = route.subagent_id.clone() {
        rec.subagent_id = Some(id.clone());
        let trajectory = subagent_trajectory_path(&route.storage_session_id);
        rec.payload["subagent_trajectory"] = json!(trajectory);
        if let Some(parent) = extract_claude_parent_agent_id(headers) {
            rec.parent_agent_id = Some(parent.clone());
            rec.payload["parent_agent_id"] = json!(parent);
        }
        outcome.spawn_link_backfills = registry.on_subagent_registered(&run_key, &id);
        return outcome;
    }

    if let Some(parent) = extract_claude_parent_agent_id(headers) {
        rec.parent_agent_id = Some(parent.clone());
        rec.payload["parent_agent_id"] = json!(parent);
    }

    if let Some(body) = request_body {
        let refs = extract_subagent_ids_from_request(body);
        if !refs.is_empty() {
            let trajectories = registry.trajectory_paths(&run_key, &refs);
            rec.payload["refs_subagent_ids"] = json!(refs);
            if !trajectories.is_empty() {
                rec.payload["subagent_trajectories"] = json!(trajectories);
            }
        }
    }

    if let Some(text) = assistant_text.filter(|s| !s.is_empty()) {
        let hints = extract_spawn_hints_from_assistant(text);
        if !hints.is_empty() {
            rec.payload["spawn_hints"] =
                json!(hints.iter().map(spawn_hint_to_json).collect::<Vec<_>>());
            let links = registry.remember_main_spawn(
                &run_key,
                rec.call_id.as_deref().unwrap_or(""),
                &hints,
            );
            apply_spawn_links(rec, &links);
        }
        let spawned = extract_subagent_ids_from_text(text);
        if !spawned.is_empty() {
            let trajectories = registry.trajectory_paths(&run_key, &spawned);
            merge_string_array_payload(rec, "refs_subagent_ids", &spawned);
            if !trajectories.is_empty() {
                merge_string_array_payload(rec, "subagent_trajectories", &trajectories);
            }
        }
    }
    outcome
}

fn merge_string_array_payload(rec: &mut CaptureRecord, key: &str, values: &[String]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        use axum::http::header::HeaderName;
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::try_from(*v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn ignores_cli_agent_id_flag_in_document_text() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "content": "--agent-id my-repo --session-id abc"
                }]
            }]
        });
        assert!(extract_subagent_ids_from_request(&body).is_empty());
    }

    #[test]
    fn extracts_agent_id_from_task_notification() {
        let text =
            "<task-notification>\n<task-id>aeb40a1230926b4a8</task-id>\n</task-notification>";
        assert_eq!(
            extract_subagent_ids_from_text(text),
            vec!["aeb40a1230926b4a8".to_string()]
        );
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": text}]
            }]
        });
        assert_eq!(
            extract_subagent_ids_from_request(&body),
            vec!["aeb40a1230926b4a8".to_string()]
        );
    }

    #[test]
    fn extracts_agent_id_from_tool_result_line() {
        let text = "Async agent launched.\nagentId: a483eb5f3a45995b0 done\n";
        assert_eq!(
            extract_subagent_ids_from_text(text),
            vec!["a483eb5f3a45995b0".to_string()]
        );
    }

    #[test]
    fn extracts_agent_id_from_tool_result_json() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": [{"type":"text","text":"{\"agentId\":\"a5aa661\",\"status\":\"complete\"}"}]
                }]
            }]
        });
        assert_eq!(
            extract_subagent_ids_from_request(&body),
            vec!["a5aa661".to_string()]
        );
    }

    #[test]
    fn extracts_spawn_hints_from_agent_tool_block() {
        let text = r#"Delegated.

```tool:Agent
{
  "subagent_type": "Explore",
  "prompt": "scan src/"
}
```"#;
        let hints = extract_spawn_hints_from_assistant(text);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].subagent_type, "Explore");
        assert_eq!(hints[0].prompt.as_deref(), Some("scan src/"));
    }

    #[test]
    fn match_spawns_by_doc_target_when_subagents_registered_first() {
        let mut registry = SubagentRegistry::default();
        let run_key = "run-20260524-021032";
        let doc = "docs/src/design/cli_capture_command.zh.md";
        let sub_body = json!({
            "messages": [{
                "role": "user",
                "content": format!("Review the Chinese design document at /Users/reiase/workspace/Persisting/{doc}")
            }]
        });
        let sub_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: "agent-a583b6c00c3d06436".into(),
            subagent_id: Some("a583b6c00c3d06436".into()),
        };
        registry.register_route(run_key, &sub_route, "call-sub", Some(&sub_body));

        let assistant = format!(
            r#"Launching review.

```tool:Agent
{{
  "description": "Review cli_capture_command design doc",
  "prompt": "Review the Chinese design document at /Users/reiase/workspace/Persisting/{doc}",
  "subagent_type": "general-purpose"
}}
```"#
        );
        let hints = extract_spawn_hints_from_assistant(&assistant);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].doc_target.as_deref(), Some(doc));

        let links = registry.match_spawns_to_agents(run_key, &hints);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].subagent_id, "a583b6c00c3d06436");
        assert_eq!(links[0].subagent_trajectory, "agent-a583b6c00c3d06436.md");

        let main_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: run_key.into(),
            subagent_id: None,
        };
        let mut rec = CaptureRecord {
            seq: 3,
            source: "persisting-proxy".into(),
            kind: "llm.response.stream".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-main".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({}),
        };
        let outcome = enrich_record(
            &mut rec,
            &main_route,
            &HeaderMap::new(),
            None,
            Some(&assistant),
            &mut registry,
        );
        assert!(outcome.spawn_link_backfills.is_empty());
        assert_eq!(
            rec.payload["spawn_links"][0]["subagent_id"],
            json!("a583b6c00c3d06436")
        );
        assert_eq!(
            rec.payload["refs_subagent_ids"],
            json!(["a583b6c00c3d06436"])
        );
    }

    #[test]
    fn backfill_spawn_link_when_subagent_registers_after_assistant() {
        let mut registry = SubagentRegistry::default();
        let run_key = "run-backfill";
        let doc = "docs/src/design/capture_design.zh.md";
        let assistant = format!(
            r#"Starting.

```tool:Agent
{{
  "description": "Review capture_design",
  "prompt": "Review {doc}",
  "subagent_type": "general-purpose"
}}
```"#
        );
        let main_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: run_key.into(),
            subagent_id: None,
        };
        let mut main_rec = CaptureRecord {
            seq: 2,
            source: "t".into(),
            kind: "llm.response.stream".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-main".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({}),
        };
        enrich_record(
            &mut main_rec,
            &main_route,
            &HeaderMap::new(),
            None,
            Some(&assistant),
            &mut registry,
        );
        assert!(main_rec.payload.get("spawn_links").is_none());

        let sub_body = json!({"messages":[{"role":"user","content": format!("Review {doc}")}]});
        let sub_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: "agent-deadbeef".into(),
            subagent_id: Some("deadbeef".into()),
        };
        let mut sub_rec = CaptureRecord {
            seq: 0,
            source: "t".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-sub".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({}),
        };
        let outcome = enrich_record(
            &mut sub_rec,
            &sub_route,
            &HeaderMap::new(),
            Some(&sub_body),
            None,
            &mut registry,
        );
        assert_eq!(outcome.spawn_link_backfills.len(), 1);
        assert_eq!(outcome.spawn_link_backfills[0].parent_call_id, "call-main");
        assert_eq!(
            outcome.spawn_link_backfills[0].links[0].subagent_id,
            "deadbeef"
        );
    }

    #[test]
    fn enrich_links_main_request_to_subagent_trajectory() {
        let mut registry = SubagentRegistry::default();
        let run_key = "run-1";
        let sub_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: "agent-abc1234".into(),
            subagent_id: Some("abc1234".into()),
        };
        registry.register_route(run_key, &sub_route, "call-sub", None);

        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "content": "agentId: abc1234 done"
                }]
            }]
        });
        let main_route = CaptureRoute {
            root_session: Some(run_key.into()),
            session_id: "sess".into(),
            storage_session_id: run_key.into(),
            subagent_id: None,
        };
        let mut rec = CaptureRecord {
            seq: 0,
            source: "t".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-main".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({}),
        };
        enrich_record(
            &mut rec,
            &main_route,
            &HeaderMap::new(),
            Some(&body),
            None,
            &mut registry,
        );
        assert_eq!(rec.payload["refs_subagent_ids"], json!(["abc1234"]));
        assert_eq!(
            rec.payload["subagent_trajectories"],
            json!(["agent-abc1234.md"])
        );
    }

    #[test]
    fn enrich_subagent_record_has_self_trajectory() {
        let mut registry = SubagentRegistry::default();
        let route = CaptureRoute {
            root_session: Some("run-1".into()),
            session_id: "sess".into(),
            storage_session_id: "agent-xyz".into(),
            subagent_id: Some("xyz".into()),
        };
        let mut rec = CaptureRecord {
            seq: 0,
            source: "t".into(),
            kind: "llm.request".into(),
            timestamp: None,
            session_id: None,
            agent_id: None,
            parent_uuid: None,
            trace_id: None,
            call_id: Some("call-1".into()),
            subagent_id: None,
            parent_agent_id: None,
            branch: None,
            parent_call_id: None,
            payload: json!({}),
        };
        enrich_record(
            &mut rec,
            &route,
            &headers(&[("x-claude-code-parent-agent-id", "main01")]),
            None,
            None,
            &mut registry,
        );
        assert_eq!(rec.subagent_id.as_deref(), Some("xyz"));
        assert_eq!(rec.parent_agent_id.as_deref(), Some("main01"));
        assert_eq!(rec.payload["subagent_trajectory"], json!("agent-xyz.md"));
    }
}
