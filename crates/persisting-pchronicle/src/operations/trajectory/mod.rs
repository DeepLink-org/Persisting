//! Typed trajectory operations over pChronicle services.
//!
//! pChronicle owns **Lance** (canonical event log) and **AgenticMD** (optional
//! human-readable view). This module maps
//! protocol requests to those pChronicle operations.
//!
//! Path: `{storage}/{agent_id}/{run_id}/` with `{session_id}.md` per logical session.
//!
use crate::{
    export_story_bundle, layer_stats, materialize_lance_to_markdown, RawEventLanceStore,
    StoryCoords,
};
pub use crate::{
    TrajectoryAppendRequest, TrajectoryAppendResponse, TrajectoryExtractRequest,
    TrajectoryExtractResponse, TrajectoryMaterializeRequest, TrajectoryMaterializeResponse,
    TrajectoryReplayRequest, TrajectoryReplayResponse, TrajectoryStatsRequest,
    TrajectoryStatsResponse,
};
use anyhow::Result;

fn session_from_request(
    storage: &str,
    agent_id: &str,
    session_id: &str,
    root_session_id: Option<&str>,
) -> StoryCoords {
    StoryCoords::new(
        storage,
        agent_id,
        session_id,
        root_session_id.map(str::to_string),
    )
}

pub async fn materialize_async(
    request: TrajectoryMaterializeRequest,
) -> Result<TrajectoryMaterializeResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let outcome = materialize_lance_to_markdown(&session).await?;
    Ok(TrajectoryMaterializeResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        markdown_path: outcome.markdown_path,
        event_rows: outcome.stats.source_events,
        markdown_blocks: outcome.stats.markdown_blocks,
        skipped_events: outcome.stats.skipped_events,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

pub async fn append_async(request: TrajectoryAppendRequest) -> Result<TrajectoryAppendResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let store = RawEventLanceStore;
    let accepted_records = request.records.len();
    let (dataset, note) = if request.records.is_empty() {
        (
            store.display_path(&session)?,
            "No non-empty records; storage unchanged.".to_string(),
        )
    } else {
        let outcome = store.append_events(&session, &request.records).await?;
        (
            store.display_path(&session)?,
            format!("canonical Lance event log. {}", outcome.note),
        )
    };

    Ok(TrajectoryAppendResponse {
        dataset,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        accepted_records,
        status: "ok".to_string(),
        note,
    })
}

pub async fn replay_async(request: TrajectoryReplayRequest) -> Result<TrajectoryReplayResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );

    let outcome = RawEventLanceStore
        .replay(&session, request.offset, request.limit)
        .await?;

    Ok(TrajectoryReplayResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        records: outcome.records,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

pub async fn stats_async(request: TrajectoryStatsRequest) -> Result<TrajectoryStatsResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );

    let layers = layer_stats(&session).await?;
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
    let duplicate_event_ids = duplicate_event_id_count(&session).await?;
    Ok(TrajectoryStatsResponse {
        dataset: layers.event_log_path,
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        row_count: layers.event_rows,
        manifest_revision: RawEventLanceStore.stats(&session).await?.manifest_revision,
        duplicate_event_ids,
        status: if layers.event_rows > 0 { "ok" } else { "empty" }.into(),
        note: format!(
            "Canonical Lance event log: {} row(s){projection_note}",
            layers.event_rows
        ),
    })
}

async fn duplicate_event_id_count(session: &StoryCoords) -> Result<usize> {
    let records = crate::RawEventLanceStore
        .read_events(session, 0, None)
        .await?;
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for event_id in records
        .into_iter()
        .filter_map(|record| record.identity.event_id)
    {
        *counts.entry(event_id).or_default() += 1;
    }
    Ok(counts.values().map(|count| count.saturating_sub(1)).sum())
}

pub async fn extract_async(request: TrajectoryExtractRequest) -> Result<TrajectoryExtractResponse> {
    let root_session_id = request.root_session_id.as_deref();
    let session = session_from_request(
        &request.storage,
        &request.agent_id,
        &request.session_id,
        root_session_id,
    );
    let out = std::path::Path::new(&request.out_dir);
    let outcome = export_story_bundle(&session, out, request.include_subagents)?;

    Ok(TrajectoryExtractResponse {
        storage: request.storage,
        agent_id: request.agent_id,
        session_id: request.session_id,
        out_dir: outcome.out_dir,
        files_copied: outcome.files_copied,
        status: "ok".to_string(),
        note: outcome.note,
    })
}

#[cfg(test)]
mod tests;
