//! Protocol-independent trajectory application service.

use anyhow::Result;

use crate::{
    dataset_display, detect_primary_layer, layer_stats, parse_engine_records,
    resolve_storage_for_append, resolve_storage_for_read, selection_label, story_stats_note,
    structured_store, AppendOutcome, ReplayOutcome, StorageKind, StorageSelection, StoryCoords,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendServiceOutcome {
    pub dataset: String,
    pub accepted_records: usize,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayServiceOutcome {
    pub records: Vec<String>,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatsServiceOutcome {
    pub dataset: String,
    pub row_count: usize,
    pub manifest_version: Option<u64>,
    pub status: String,
    pub note: String,
}

fn store_for(selection: StorageSelection) -> Box<dyn crate::StructuredStore> {
    match selection {
        StorageSelection::AgenticMd => structured_store(StorageKind::AgenticMd),
        StorageSelection::Lance | StorageSelection::Auto => structured_store(StorageKind::Lance),
    }
}

pub async fn append_trajectory(
    session: &StoryCoords,
    requested: StorageSelection,
    records_ronl: &str,
) -> Result<AppendServiceOutcome> {
    let lines = parse_engine_records(records_ronl)?;
    let accepted_records = lines.len();
    if accepted_records == 0 {
        return Ok(AppendServiceOutcome {
            dataset: dataset_display(session, requested)?,
            accepted_records: 0,
            status: "ok".into(),
            note: "No non-empty records; storage unchanged.".into(),
        });
    }

    let resolved = resolve_storage_for_append(session, requested).await?;
    let AppendOutcome { note, .. } = store_for(resolved).append(session, &lines).await?;
    Ok(AppendServiceOutcome {
        dataset: dataset_display(session, resolved)?,
        accepted_records,
        status: "ok".into(),
        note: format!("storage_format={}. {note}", selection_label(resolved)),
    })
}

pub async fn replay_trajectory(
    session: &StoryCoords,
    requested: StorageSelection,
    offset: usize,
    limit: Option<usize>,
) -> Result<ReplayServiceOutcome> {
    let resolved = resolve_storage_for_read(session, requested).await?;
    let ReplayOutcome { records, note } =
        store_for(resolved).replay(session, offset, limit).await?;
    Ok(ReplayServiceOutcome {
        records,
        status: "ok".into(),
        note,
    })
}

pub async fn trajectory_stats(
    session: &StoryCoords,
    requested: StorageSelection,
) -> Result<StatsServiceOutcome> {
    if matches!(requested, StorageSelection::Auto) {
        let layers = layer_stats(session).await?;
        let primary = detect_primary_layer(&layers, session);
        let row_count = match primary {
            StorageSelection::AgenticMd => layers.markdown_blocks,
            _ => layers.event_rows,
        };
        let dataset = match primary {
            StorageSelection::AgenticMd => layers
                .markdown_path
                .clone()
                .unwrap_or_else(|| layers.event_log_path.clone()),
            _ => layers.event_log_path.clone(),
        };
        return Ok(StatsServiceOutcome {
            dataset,
            row_count,
            manifest_version: None,
            status: if row_count > 0 { "ok" } else { "empty" }.into(),
            note: story_stats_note(&layers, primary),
        });
    }

    let resolved = resolve_storage_for_read(session, requested).await?;
    let stats = store_for(resolved).stats(session).await?;
    Ok(StatsServiceOutcome {
        dataset: stats.dataset,
        row_count: stats.row_count,
        manifest_version: stats.manifest_version,
        status: stats.status,
        note: stats.note,
    })
}
