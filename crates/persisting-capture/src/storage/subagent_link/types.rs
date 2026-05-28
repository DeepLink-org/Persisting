//! Spawn-link value types.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnHint {
    pub subagent_type: String,
    pub description: Option<String>,
    pub prompt: Option<String>,
    pub doc_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnLink {
    pub subagent_type: String,
    pub description: Option<String>,
    pub doc_target: Option<String>,
    pub subagent_id: String,
    pub subagent_trajectory: String,
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
pub(crate) struct PendingMainSpawn {
    pub(crate) parent_call_id: String,
    pub(crate) hints: Vec<SpawnHint>,
    pub(crate) links: Vec<SpawnLink>,
    pub(crate) matched_agents: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RunSubagents {
    pub(crate) agents: HashMap<String, AgentEntry>,
    pub(crate) pending_mains: Vec<PendingMainSpawn>,
    pub(crate) main_storage_session_id: Option<String>,
}

/// Tracks subagent instances seen during one `capture run` root session.
#[derive(Debug, Default)]
pub struct SubagentRegistry {
    pub(crate) runs: HashMap<String, RunSubagents>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnLinkBackfill {
    pub parent_call_id: String,
    pub links: Vec<SpawnLink>,
}

/// Side effects from [`enrich_record`](super::enrich::enrich_record).
#[derive(Debug, Clone, Default)]
pub struct EnrichOutcome {
    pub spawn_link_backfills: Vec<SpawnLinkBackfill>,
}

pub fn subagent_trajectory_path(storage_leaf: &str) -> String {
    format!("{storage_leaf}.md")
}
