//! Protocol-independent trajectory application service.

use anyhow::Result;

use crate::{
    layer_stats, AppendOutcome, EventRecord, RawEventLanceStore, ReplayOutcome, StoryCoords,
    StructuredStore, TrajectoryStorageFormat,
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

pub async fn append_trajectory(
    session: &StoryCoords,
    _requested: TrajectoryStorageFormat,
    records: &[EventRecord],
) -> Result<AppendServiceOutcome> {
    let store = RawEventLanceStore;
    let accepted_records = records.len();
    if accepted_records == 0 {
        return Ok(AppendServiceOutcome {
            dataset: store.display_path(session)?,
            accepted_records: 0,
            status: "ok".into(),
            note: "No non-empty records; storage unchanged.".into(),
        });
    }

    let AppendOutcome { note, .. } = store.append_events(session, records).await?;
    Ok(AppendServiceOutcome {
        dataset: store.display_path(session)?,
        accepted_records,
        status: "ok".into(),
        note: format!("storage_format=lance (canonical). {note}"),
    })
}

pub async fn replay_trajectory(
    session: &StoryCoords,
    _requested: TrajectoryStorageFormat,
    offset: usize,
    limit: Option<usize>,
) -> Result<ReplayServiceOutcome> {
    let ReplayOutcome { records, note } = RawEventLanceStore.replay(session, offset, limit).await?;
    Ok(ReplayServiceOutcome {
        records,
        status: "ok".into(),
        note,
    })
}

pub async fn trajectory_stats(
    session: &StoryCoords,
    _requested: TrajectoryStorageFormat,
) -> Result<StatsServiceOutcome> {
    let layers = layer_stats(session).await?;
    let projection_note = if layers.markdown_blocks > 0 {
        format!(
            "; AgenticMD debug view {} block(s){}",
            layers.markdown_blocks,
            layers
                .markdown_path
                .as_deref()
                .map(|path| format!(" at {path}"))
                .unwrap_or_default()
        )
    } else {
        "; no AgenticMD debug view".to_string()
    };
    Ok(StatsServiceOutcome {
        dataset: layers.event_log_path,
        row_count: layers.event_rows,
        manifest_version: None,
        status: if layers.event_rows > 0 { "ok" } else { "empty" }.into(),
        note: format!(
            "Canonical Lance event log: {} row(s){projection_note}",
            layers.event_rows
        ),
    })
}
