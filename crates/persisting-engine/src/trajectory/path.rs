//! Path inference re-exported from capture (Run/Story layout).

pub use persisting_capture::path_layout::{
    list_story_read_locations, list_traj_read_locations, merge_story_location, merge_traj_location,
    resolve_story_read_location, resolve_traj_read_location, try_infer_story_location,
    try_infer_traj_location, StoryLocationPartial, StoryLocationPartial as TrajLocationPartial,
};
pub use persisting_capture::StoryCoords;
pub type TrajLocation = StoryCoords;
