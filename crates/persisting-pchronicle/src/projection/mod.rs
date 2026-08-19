//! Rebuildable views derived from canonical trajectory facts.

mod storyline;

pub use storyline::{
    build_storyline_projection, projection_lineage_is_fresh, rebuild_storyline_projection,
    storyline_projection_status, sync_storyline_projection, verify_storyline_projection,
    ProjectionRebuildReason, StorylineProjectionBuildOutcome, StorylineProjectionBuildReport,
    StorylineProjectionStatus, StorylineProjectionSyncMode, StorylineProjectionSyncOutcome,
    StorylineProjectionSyncReport, StorylineProjectionVerification,
};
