//! Enrich capture records with subagent cross-refs.

use axum::http::HeaderMap;
use serde_json::{json, Value};

use super::extract::{
    extract_spawn_hints_from_assistant, extract_subagent_ids_from_request,
    extract_subagent_ids_from_text,
};
use super::match_::{
    apply_spawn_links, merge_string_array_payload, spawn_hint_to_json, spawn_link_to_json,
};
use super::types::{EnrichOutcome, SpawnLink, SubagentRegistry};
use crate::record::CaptureRecord;
use crate::session_chain::extract_claude_parent_agent_id;
use crate::session_storage::CaptureRoute;

pub fn spawn_link_backfill_record(
    parent_call_id: &str,
    links: &[SpawnLink],
    call: &crate::Call,
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
        let trajectory = super::types::subagent_trajectory_path(&route.storage_session_id);
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
