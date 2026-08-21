//! Rebuildable views derived from canonical trajectory facts.

mod automatic;
mod storyline;

pub use automatic::{
    automatic_projection_inventory, inspect_automatic_storyline_projection,
    probe_canonical_event_store, storyline_projection_destination_exists,
    AutomaticProjectionInspection, AutomaticProjectionInventory, AutomaticProjectionInventoryError,
    AutomaticProjectionState, AutomaticProjectionTarget,
};
pub use storyline::{
    build_storyline_projection, projection_lineage_is_fresh, rebuild_storyline_projection,
    storyline_projection_status, sync_storyline_projection, verify_storyline_projection,
    ProjectionRebuildReason, StorylineProjectionBuildOutcome, StorylineProjectionBuildReport,
    StorylineProjectionStatus, StorylineProjectionSyncMode, StorylineProjectionSyncOutcome,
    StorylineProjectionSyncReport, StorylineProjectionVerification,
};
