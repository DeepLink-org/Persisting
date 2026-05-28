//! Main agent ↔ subagent cross-reference for trajectory capture.

mod enrich;
mod extract;
mod match_;
mod registry;
mod types;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

pub use enrich::{
    enrich_record, main_route_for_backfill, run_registry_key, spawn_link_backfill_record,
};
pub use extract::{
    append_subagent_refs_footer, extract_spawn_hints_from_assistant,
    extract_subagent_ids_from_request, extract_subagent_ids_from_text,
};
pub use types::subagent_trajectory_path;
pub use types::{
    AgentEntry, EnrichOutcome, SpawnHint, SpawnLink, SpawnLinkBackfill, SubagentRegistry,
};
