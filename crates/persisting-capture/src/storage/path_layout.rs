//! Resolve Run/Story paths from CLI storage arguments (capture egress).
//!
//! Accepts a storage root (`store/`), agent directory (`store/{agent_id}/`),
//! or session directory (`store/{agent_id}/{session_id}/`, nested subagent paths).

use std::path::{Component, Path};

use super::markdown::{locate_session_markdown, session_markdown_path_for_key};
use super::story_coords::StoryCoords;
use anyhow::Result;

pub type TrajLocation = StoryCoords;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryLocationPartial {
    pub storage: String,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedTrajPath {
    storage: String,
    agent_id: String,
    session_id: String,
    root_session_id: Option<String>,
}

fn path_components(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

fn join_storage_prefix(components: &[String], end: usize) -> String {
    components[..end].join("/")
}

fn normalized_path_arg(path_arg: &str) -> String {
    path_arg.trim().trim_end_matches('/').to_string()
}

/// Infer `{storage, agent_id, session_id[, root_session_id]}` from a session directory path.
fn infer_traj_location_from_path(path_arg: &str) -> Option<ParsedTrajPath> {
    let trimmed = normalized_path_arg(path_arg);
    if trimmed.is_empty() {
        return None;
    }
    let session_dir = Path::new(&trimmed);
    if !looks_like_session_dir(session_dir) {
        return None;
    }

    let session_id = session_dir.file_name()?.to_string_lossy().into_owned();
    let parent = session_dir.parent()?;

    if parent.file_name().is_some_and(|n| n == "subagents") {
        let root_dir = parent.parent()?;
        let root_session_id = root_dir.file_name()?.to_string_lossy().into_owned();
        let agent_dir = root_dir.parent()?;
        let agent_id = agent_dir.file_name()?.to_string_lossy().into_owned();
        let storage = agent_dir.parent()?.to_string_lossy().into_owned();
        return Some(ParsedTrajPath {
            storage,
            agent_id,
            root_session_id: Some(root_session_id),
            session_id,
        });
    }

    let agent_dir = parent;
    let agent_id = agent_dir.file_name()?.to_string_lossy().into_owned();
    let storage = agent_dir.parent()?.to_string_lossy().into_owned();
    Some(ParsedTrajPath {
        storage,
        agent_id,
        session_id: session_id.clone(),
        root_session_id: infer_root_session_id(session_dir, &session_id),
    })
}

fn looks_like_session_dir(dir: &Path) -> bool {
    dir.is_dir()
        && (locate_session_markdown(dir).is_some()
            || dir.join("_versions").is_dir()
            || dir.join(".lance").is_dir()
            || dir.join(".capture").join("run_session").is_file())
}

/// Capture run layout: `{storage}/{agent}/{run_id}/.lance/{run_id}/`.
/// Flat layout: Lance dataset lives directly under `{storage}/{agent}/{session_id}/`.
fn infer_root_session_id(session_dir: &Path, session_id: &str) -> Option<String> {
    if session_dir.join(".lance").join(session_id).is_dir()
        || session_dir.join("_versions").is_dir()
    {
        Some(session_id.to_string())
    } else {
        None
    }
}

fn list_session_dirs(agent_dir: &Path) -> Option<Vec<String>> {
    let read_dir = std::fs::read_dir(agent_dir).ok()?;
    let mut sessions = Vec::new();
    for entry in read_dir.flatten() {
        let ft = entry.file_type().ok()?;
        if !ft.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "subagents" {
            continue;
        }
        if looks_like_session_dir(&entry.path()) {
            sessions.push(name);
        }
    }
    Some(sessions)
}

/// `{storage}/{agent_id}` with a single session subdirectory.
fn infer_from_agent_dir(path_arg: &str) -> Option<ParsedTrajPath> {
    let trimmed = normalized_path_arg(path_arg);
    let path = Path::new(&trimmed);
    let components = path_components(path);
    if components.len() < 2 || !path.is_dir() {
        return None;
    }
    let n = components.len();
    let agent_id = components[n - 1].clone();
    let storage = join_storage_prefix(&components, n - 1);
    let sessions = list_session_dirs(path)?;
    if sessions.len() == 1 {
        let session_id = sessions.into_iter().next()?;
        let session_dir = path.join(&session_id);
        return Some(ParsedTrajPath {
            storage,
            agent_id,
            session_id: session_id.clone(),
            root_session_id: infer_root_session_id(&session_dir, &session_id),
        });
    }
    None
}

/// Storage root with exactly one session anywhere under `{storage}/{agent_id}/`.
fn infer_from_storage_root(path_arg: &str) -> Option<ParsedTrajPath> {
    let trimmed = normalized_path_arg(path_arg);
    let path = Path::new(&trimmed);
    if !path.is_dir() {
        return None;
    }
    let read_dir = std::fs::read_dir(path).ok()?;
    let mut found = Vec::new();
    for agent_entry in read_dir.flatten() {
        if !agent_entry.file_type().ok()?.is_dir() {
            continue;
        }
        let agent_id = agent_entry.file_name().to_string_lossy().into_owned();
        if agent_id.starts_with('.') {
            continue;
        }
        let Some(sessions) = list_session_dirs(&agent_entry.path()) else {
            continue;
        };
        for session_id in sessions {
            let session_dir = agent_entry.path().join(&session_id);
            found.push(ParsedTrajPath {
                storage: trimmed.clone(),
                agent_id: agent_id.clone(),
                session_id: session_id.clone(),
                root_session_id: infer_root_session_id(&session_dir, &session_id),
            });
        }
    }
    if found.len() == 1 {
        return found.into_iter().next();
    }
    None
}

pub type TrajLocationPartial = StoryLocationPartial;

fn parsed_to_location(parsed: ParsedTrajPath) -> StoryCoords {
    StoryCoords {
        storage: parsed.storage,
        agent_id: parsed.agent_id,
        session_id: parsed.session_id,
        root_session_id: parsed.root_session_id,
    }
}

/// Best-effort inference from a path (no CLI flags).
pub fn try_infer_story_location(path_arg: &str) -> Option<StoryCoords> {
    infer_traj_location_from_path(path_arg)
        .or_else(|| infer_from_agent_dir(path_arg))
        .or_else(|| infer_from_storage_root(path_arg))
        .map(parsed_to_location)
}

/// Merge path inference with explicit flags; missing ids stay unset.
pub fn merge_story_location(
    path_arg: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> StoryLocationPartial {
    let inferred = try_infer_story_location(&path_arg);
    StoryLocationPartial {
        storage: inferred
            .as_ref()
            .map(|p| p.storage.clone())
            .unwrap_or(path_arg),
        agent_id: agent_id.or_else(|| inferred.as_ref().map(|p| p.agent_id.clone())),
        session_id: session_id.or_else(|| inferred.as_ref().map(|p| p.session_id.clone())),
        root_session_id: root_session_id
            .or_else(|| inferred.as_ref().and_then(|p| p.root_session_id.clone())),
    }
}

/// When a capture run uses a header session id (UUID) for Lance/md but the run folder is `run-*`,
/// remap the story storage key from the primary markdown stem under the run directory.
fn remap_capture_run_story_session_id(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> String {
    let Some(root) = root_session_id else {
        return session_id.to_string();
    };
    if session_id != root {
        return session_id.to_string();
    }
    let run_dir = Path::new(storage).join(agent_id).join(root);
    primary_story_storage_key(&run_dir, root).unwrap_or_else(|| session_id.to_string())
}

fn primary_story_storage_key(run_dir: &Path, run_id: &str) -> Option<String> {
    if session_markdown_path_for_key(run_dir, run_id).is_file() {
        return None;
    }
    let md = locate_session_markdown(run_dir)?;
    let stem = md.file_stem()?.to_str()?;
    if stem == run_id {
        return None;
    }
    Some(stem.to_string())
}

/// Read commands (`replay`, `stats`) require a resolved session.
pub fn resolve_story_read_location(
    op: &str,
    path_arg: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> Result<StoryCoords> {
    let partial = merge_story_location(path_arg, agent_id, session_id, root_session_id);
    match (partial.agent_id, partial.session_id) {
        (Some(agent_id), Some(session_id)) => {
            let root_session_id = partial.root_session_id.or_else(|| {
                infer_root_session_id(
                    Path::new(&partial.storage)
                        .join(&agent_id)
                        .join(&session_id)
                        .as_path(),
                    &session_id,
                )
            });
            let session_id = remap_capture_run_story_session_id(
                &partial.storage,
                &agent_id,
                &session_id,
                root_session_id.as_deref(),
            );
            Ok(StoryCoords {
                storage: partial.storage,
                agent_id,
                session_id,
                root_session_id,
            })
        }
        _ => anyhow::bail!(
            "{op}: 请指定 session 目录（如 `store/{{agent_id}}/{{session_id}}/`），或同时提供 --agent-id 与 --session-id"
        ),
    }
}

/// Legacy aliases for engine / CLI compatibility.
pub use merge_story_location as merge_traj_location;
pub use resolve_story_read_location as resolve_traj_read_location;
pub use try_infer_story_location as try_infer_traj_location;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_flat_session_path() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-flat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base
            .join("store")
            .join("deepseek-proxy")
            .join("run-20260524-015351-928492000");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let p = infer_traj_location_from_path(session.to_str().unwrap()).unwrap();
        assert!(p.storage.ends_with("store"));
        assert_eq!(p.agent_id, "deepseek-proxy");
        assert_eq!(p.session_id, "run-20260524-015351-928492000");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn infer_flat_session_path_string_only() {
        let base = std::env::temp_dir().join(format!("persisting-traj-rel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base.join("store").join("a").join("s");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();
        assert!(infer_traj_location_from_path("store/a/s").is_none());
        let p = infer_traj_location_from_path(session.to_str().unwrap()).unwrap();
        assert_eq!(p.agent_id, "a");
        assert_eq!(p.session_id, "s");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn infer_nested_subagent_path() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-nest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base
            .join("store")
            .join("agent")
            .join("root-run")
            .join("subagents")
            .join("sub-uuid");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let p = infer_traj_location_from_path(session.to_str().unwrap()).unwrap();
        assert!(p.storage.ends_with("store"));
        assert_eq!(p.agent_id, "agent");
        assert_eq!(p.root_session_id.as_deref(), Some("root-run"));
        assert_eq!(p.session_id, "sub-uuid");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn infer_storage_root_only_returns_none_without_scan() {
        assert!(infer_traj_location_from_path("store").is_none());
    }

    #[test]
    fn resolve_merges_explicit_flags_with_inferred_storage() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base.join("store").join("deepseek-proxy").join("run-abc");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let loc = resolve_traj_read_location(
            "trajectory stats",
            session.to_str().unwrap().into(),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(loc.storage.ends_with("store"));
        assert_eq!(loc.agent_id, "deepseek-proxy");
        assert_eq!(loc.session_id, "run-abc");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_infers_root_for_capture_run_layout() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-run-layout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base.join("store").join("agent").join("run-1");
        let lance = session.join(".lance").join("run-1");
        std::fs::create_dir_all(&lance).unwrap();
        std::fs::write(session.join("run-1.md"), "# test\n").unwrap();

        let loc = resolve_traj_read_location(
            "trajectory stats",
            session.to_str().unwrap().into(),
            None,
            None,
            None,
        )
        .unwrap();
        assert!(loc.storage.ends_with("store"));
        assert_eq!(loc.agent_id, "agent");
        assert_eq!(loc.session_id, "run-1");
        assert_eq!(loc.root_session_id.as_deref(), Some("run-1"));

        let loc = resolve_traj_read_location(
            "trajectory stats",
            session.to_str().unwrap().into(),
            Some("agent".into()),
            Some("run-1".into()),
            None,
        )
        .unwrap();
        assert_eq!(loc.root_session_id.as_deref(), Some("run-1"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_capture_run_remaps_to_primary_story_stem() {
        let base = std::env::temp_dir().join(format!(
            "persisting-traj-story-stem-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let session = base.join("store").join("agent").join("run-1");
        std::fs::create_dir_all(session.join(".lance").join("run-1")).unwrap();
        std::fs::create_dir_all(session.join(".lance").join("uuid-story")).unwrap();
        std::fs::write(session.join("uuid-story.md"), "# test\n").unwrap();

        let loc = resolve_traj_read_location(
            "trajectory stats",
            session.to_str().unwrap().into(),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(loc.session_id, "uuid-story");
        assert_eq!(loc.root_session_id.as_deref(), Some("run-1"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn resolve_real_capture_run_store_if_present() {
        let session = Path::new("store/deepseek-proxy/run-20260529-020451-705391000");
        if !session.is_dir() {
            return;
        }
        let loc = resolve_traj_read_location(
            "trajectory stats",
            session.to_str().unwrap().into(),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            loc.session_id,
            "5e0dfcdb-56ee-49d1-8921-4aeefeea3b17",
            "expected header-session stem, got {}",
            loc.session_id
        );
    }

    #[test]
    fn resolve_explicit_ids_with_storage_root() {
        let loc = resolve_traj_read_location(
            "trajectory stats",
            "store".into(),
            Some("a".into()),
            Some("s".into()),
            None,
        )
        .unwrap();
        assert_eq!(loc.storage, "store");
        assert_eq!(loc.agent_id, "a");
        assert_eq!(loc.session_id, "s");
    }

    #[test]
    fn resolve_fails_without_ids_or_deep_path() {
        assert!(
            resolve_traj_read_location("trajectory stats", "store".into(), None, None, None,)
                .is_err()
        );
    }

    #[test]
    fn infer_from_agent_dir_single_session() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let agent = base.join("store").join("deepseek-proxy");
        let session = agent.join("run-only");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let p = infer_from_agent_dir(agent.to_str().unwrap()).unwrap();
        assert!(p.storage.ends_with("store"));
        assert_eq!(p.agent_id, "deepseek-proxy");
        assert_eq!(p.session_id, "run-only");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn infer_from_storage_root_single_session() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let store = base.join("store");
        let session = store.join("agent-a").join("run-one");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let loc = try_infer_story_location(store.to_str().unwrap()).unwrap();
        assert!(loc.storage.ends_with("store"));
        assert_eq!(loc.agent_id, "agent-a");
        assert_eq!(loc.session_id, "run-one");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn merge_partial_flags_with_inferred_path() {
        let base =
            std::env::temp_dir().join(format!("persisting-traj-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let session = base.join("store").join("a").join("s");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::write(session.join("0001.md"), "# test\n").unwrap();

        let loc = merge_traj_location(
            session.to_str().unwrap().into(),
            Some("override-agent".into()),
            None,
            None,
        );
        assert!(loc.storage.ends_with("store"));
        assert_eq!(loc.agent_id.as_deref(), Some("override-agent"));
        assert_eq!(loc.session_id.as_deref(), Some("s"));

        let _ = std::fs::remove_dir_all(&base);
    }
}
