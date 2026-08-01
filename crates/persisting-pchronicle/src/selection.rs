//! Storage-layer selection and presentation policy.

use crate::{
    locate_session_markdown_for_key, session_lance_path, AgenticMdStore, LanceEventStore,
    LayerStats, StoryCoords, StructuredStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageSelection {
    Auto,
    Lance,
    AgenticMd,
}

pub fn detect_primary_layer(layers: &LayerStats, session: &StoryCoords) -> StorageSelection {
    match (layers.event_rows > 0, layers.markdown_blocks > 0) {
        (false, true) => StorageSelection::AgenticMd,
        (true, false) => StorageSelection::Lance,
        (false, false) => StorageSelection::AgenticMd,
        (true, true) if session.root_session_id.is_some() => StorageSelection::AgenticMd,
        (true, true) if layers.markdown_blocks < layers.event_rows => StorageSelection::AgenticMd,
        (true, true) => StorageSelection::Lance,
    }
}

async fn resolve_auto(
    session: &StoryCoords,
    when_empty: StorageSelection,
    when_both: StorageSelection,
) -> anyhow::Result<StorageSelection> {
    let has_lance = LanceEventStore.exists(session).await?;
    let has_markdown = AgenticMdStore.exists(session).await?;
    Ok(match (has_lance, has_markdown) {
        (true, false) => StorageSelection::Lance,
        (false, true) => StorageSelection::AgenticMd,
        (false, false) => when_empty,
        (true, true) => when_both,
    })
}

pub async fn resolve_for_read(
    session: &StoryCoords,
    requested: StorageSelection,
) -> anyhow::Result<StorageSelection> {
    match requested {
        StorageSelection::Auto => {
            resolve_auto(session, StorageSelection::Lance, StorageSelection::Lance).await
        }
        explicit => Ok(explicit),
    }
}

pub async fn resolve_for_append(
    session: &StoryCoords,
    requested: StorageSelection,
) -> anyhow::Result<StorageSelection> {
    match requested {
        StorageSelection::Auto => {
            resolve_auto(session, StorageSelection::Lance, StorageSelection::Lance).await
        }
        explicit => Ok(explicit),
    }
}

pub fn selection_label(selection: StorageSelection) -> &'static str {
    match selection {
        StorageSelection::Auto => "auto",
        StorageSelection::Lance => "lance",
        StorageSelection::AgenticMd => "markdown",
    }
}

pub fn dataset_display(
    session: &StoryCoords,
    selection: StorageSelection,
) -> anyhow::Result<String> {
    let run_dir = session.run_dir()?;
    match selection {
        StorageSelection::AgenticMd => Ok(locate_session_markdown_for_key(
            &run_dir,
            &session.session_id,
        )
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| run_dir.display().to_string())),
        _ => session_lance_path(session).map(|path| path.display().to_string()),
    }
}

pub fn story_stats_note(layers: &LayerStats, primary: StorageSelection) -> String {
    let primary_count = match primary {
        StorageSelection::AgenticMd => layers.markdown_blocks,
        _ => layers.event_rows,
    };
    match (layers.event_rows > 0, layers.markdown_blocks > 0) {
        (true, true) => format!(
            "Story stats via {via} ({primary_count}); Lance {} raw event(s), Markdown {} dialogue block(s)",
            layers.event_rows,
            layers.markdown_blocks,
            via = selection_label(primary)
        ),
        (true, false) => format!("Story stats via lance ({primary_count} raw event(s))"),
        (false, true) => format!("Story stats via markdown ({primary_count} dialogue block(s))"),
        (false, false) => "Story stats: no trajectory data".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_run_prefers_human_projection_when_both_layers_exist() {
        let layers = LayerStats {
            event_rows: 30,
            markdown_blocks: 30,
            event_log_path: "events.lance".into(),
            markdown_path: Some("run.md".into()),
            note: String::new(),
        };
        let session = StoryCoords::new("store", "agent", "run", Some("run".into()));
        assert_eq!(
            detect_primary_layer(&layers, &session),
            StorageSelection::AgenticMd
        );
    }
}
