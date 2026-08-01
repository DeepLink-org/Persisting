//! On-disk layout for agent trajectory storage (run dirs, markdown, Lance paths).

mod coords;
mod markdown;
mod resolve;

pub use coords::{story_lance_event_path, story_run_dir, StoryCoords};
pub use markdown::{
    is_subagent_session_storage_key, is_trajectory_markdown_path, locate_run_bucket_markdown,
    locate_session_markdown, locate_session_markdown_for_key, sanitize_session_filename,
    session_markdown_filename, session_markdown_path_for_key, session_markdown_write_path_for_key,
};
pub use resolve::{
    list_story_read_locations, list_traj_read_locations, merge_story_location, merge_traj_location,
    resolve_story_read_location, resolve_traj_read_location, try_infer_story_location,
    try_infer_traj_location, StoryLocationPartial, TrajLocation, TrajLocationPartial,
};
