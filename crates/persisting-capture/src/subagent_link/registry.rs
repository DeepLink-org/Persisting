//! Per-run subagent registry and pending spawn matching.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::extract::extract_doc_target;
use super::match_::{
    find_agent_for_spawn, first_user_message_from_json, merge_spawn_hints, spawn_hint_matches_link,
};
use super::types::{
    subagent_trajectory_path, AgentEntry, PendingMainSpawn, RunSubagents, SpawnHint, SpawnLink,
    SpawnLinkBackfill, SubagentRegistry,
};
use crate::session_storage::CaptureRoute;

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

    pub(crate) fn remember_main_spawn(
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

    pub(crate) fn on_subagent_registered(
        &mut self,
        run_key: &str,
        agent_id: &str,
    ) -> Vec<SpawnLinkBackfill> {
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
