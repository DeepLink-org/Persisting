//! Read turn-count index from `.capture/story_snapshots.json` without engine types.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

fn snapshots_path(storage: &Path) -> PathBuf {
    storage.join(".capture").join("story_snapshots.json")
}

/// Per markdown session stem: user-visible dialogue turn count.
pub fn load_snapshot_turn_counts(storage: &Path) -> Result<HashMap<String, u64>> {
    let path = snapshots_path(storage);
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let root: Value = serde_json::from_str(&raw).context("parse story_snapshots.json")?;
    if let Some(counts) = root.get("turn_counts").and_then(Value::as_object) {
        let mut out = HashMap::new();
        for (stem, v) in counts {
            if let Some(n) = v.as_u64() {
                out.insert(stem.clone(), n);
            }
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    let mut out = HashMap::new();
    if let Some(stories) = root.get("stories").and_then(Value::as_object) {
        for (stem, story) in stories {
            out.insert(stem.clone(), user_turn_count_from_story_json(story));
        }
    }
    Ok(out)
}

fn user_turn_count_from_story_json(story: &Value) -> u64 {
    let Some(turns) = story.get("turns").and_then(Value::as_array) else {
        return 0;
    };
    turns
        .iter()
        .filter(|t| {
            t.get("kind").and_then(|k| k.as_str()) == Some("dialogue") && t.get("user").is_some()
        })
        .count() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn load_prefers_turn_counts_field() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join(".capture");
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(
            capture.join("story_snapshots.json"),
            json!({
                "updated_at": "t",
                "turn_counts": { "run-a": 3 },
                "stories": { "run-a": { "turns": [] } }
            })
            .to_string(),
        )
        .unwrap();
        let counts = load_snapshot_turn_counts(dir.path()).unwrap();
        assert_eq!(counts.get("run-a"), Some(&3));
    }

    #[test]
    fn load_falls_back_to_stories_array() {
        let dir = tempfile::tempdir().unwrap();
        let capture = dir.path().join(".capture");
        std::fs::create_dir_all(&capture).unwrap();
        std::fs::write(
            capture.join("story_snapshots.json"),
            json!({
                "updated_at": "t",
                "stories": {
                    "run-a": {
                        "turns": [
                            { "kind": "dialogue", "user": { "text": "hi" } },
                            { "kind": "autonomous" }
                        ]
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        let counts = load_snapshot_turn_counts(dir.path()).unwrap();
        assert_eq!(counts.get("run-a"), Some(&1));
    }
}
