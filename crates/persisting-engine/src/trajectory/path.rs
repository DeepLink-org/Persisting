//! Path inference from pChronicle (Run/Story layout).

pub use persisting_pchronicle::{
    list_story_read_locations, list_traj_read_locations, merge_story_location, merge_traj_location,
    resolve_story_read_location, resolve_traj_read_location, try_infer_story_location,
    try_infer_traj_location, StoryCoords, StoryLocationPartial,
    StoryLocationPartial as TrajLocationPartial,
};
pub type TrajLocation = StoryCoords;
