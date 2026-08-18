//! Discover logical story partitions inside pChronicle storage.

use anyhow::Result;

use crate::layout::StoryCoords;
use crate::store::distinct_session_ids_in_run;

fn is_shared_lance_run_bucket(location: &StoryCoords) -> bool {
    location
        .root_session_id
        .as_deref()
        .is_some_and(|root| root == location.session_id)
}

/// Expand capture run buckets into one coordinate per stored `session_id`.
pub async fn expand_story_locations(locations: Vec<StoryCoords>) -> Result<Vec<StoryCoords>> {
    let mut expanded = Vec::new();
    for location in locations {
        if !is_shared_lance_run_bucket(&location) {
            expanded.push(location);
            continue;
        }
        let session_ids = distinct_session_ids_in_run(&location).await?;
        if session_ids.is_empty() {
            expanded.push(location);
            continue;
        }
        for session_id in session_ids {
            expanded.push(StoryCoords::new(
                location.storage.clone(),
                location.agent_id.clone(),
                session_id,
                location.root_session_id.clone(),
            ));
        }
    }
    expanded.sort_by(|a, b| {
        (
            a.storage.as_str(),
            a.agent_id.as_str(),
            a.root_session_id.as_deref().unwrap_or(""),
            a.session_id.as_str(),
        )
            .cmp(&(
                b.storage.as_str(),
                b.agent_id.as_str(),
                b.root_session_id.as_deref().unwrap_or(""),
                b.session_id.as_str(),
            ))
    });
    Ok(expanded)
}

/// Drop run-id lifecycle partitions when the same run contains real story partitions.
pub fn drop_lifecycle_run_partitions(locations: Vec<StoryCoords>) -> Vec<StoryCoords> {
    use std::collections::{HashMap, HashSet};

    let mut groups: HashMap<(String, String, Option<String>), Vec<usize>> = HashMap::new();
    for (index, location) in locations.iter().enumerate() {
        groups
            .entry((
                location.storage.clone(),
                location.agent_id.clone(),
                location.root_session_id.clone(),
            ))
            .or_default()
            .push(index);
    }

    let mut drop = HashSet::new();
    for indices in groups.values() {
        if indices.len() <= 1 {
            continue;
        }
        for &index in indices {
            let location = &locations[index];
            if location
                .root_session_id
                .as_deref()
                .is_some_and(|root| root == location.session_id)
            {
                drop.insert(index);
            }
        }
    }

    locations
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !drop.contains(index))
        .map(|(_, location)| location)
        .collect()
}

pub fn expand_story_locations_blocking(locations: Vec<StoryCoords>) -> Result<Vec<StoryCoords>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(expand_story_locations(locations))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_lifecycle_partition_when_story_partition_exists() {
        let locations = vec![
            StoryCoords::new("store", "agent", "run-1", Some("run-1".into())),
            StoryCoords::new("store", "agent", "story-1", Some("run-1".into())),
        ];
        let kept = drop_lifecycle_run_partitions(locations);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].session_id, "story-1");
    }
}
