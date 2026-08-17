//! Rebuildable views derived from canonical trajectory facts.

mod storyline;

pub use crate::agenticmd::{
    event_records_to_storyline, layer_stats, materialize_lance_to_markdown,
    materialize_markdown_path, write_markdown_projection, LayerStats, MaterializeOutcome,
    MaterializeStats,
};
pub use storyline::{
    build_storyline_projection, canonical_projection_lineage, projection_lineage_is_fresh,
    rebuild_storyline_projection, storyline_projection_status, sync_storyline_projection,
    verify_storyline_projection, StorylineProjectionBuildReport, StorylineProjectionStatus,
    StorylineProjectionSyncMode, StorylineProjectionSyncReport, StorylineProjectionVerification,
    STORYLINE_PROJECTION_COMPLETENESS, STORYLINE_PROJECTOR_NAME,
};
