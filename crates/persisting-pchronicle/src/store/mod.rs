//! pChronicle storage, separated by logical data model.
//!
//! - `events`: canonical append/replay storage for `EventRecord`.
//! - `storyline`: normalized three-table projection for `StorylineDocument`.
//! - `search`: document retrieval storage lives outside this module.

#[cfg(feature = "lance-store")]
use anyhow::Context as _;

#[cfg(feature = "lance-store")]
mod agenticmd_datafusion;
#[cfg(feature = "lance-store")]
mod attempt_registry;
#[cfg(feature = "lance-store")]
mod catalog;
#[cfg(feature = "lance-store")]
mod datafusion_bridge;
#[cfg(feature = "lance-store")]
pub(crate) mod dataset_write_lock;
#[cfg(feature = "lance-store")]
mod document_source;
#[cfg(feature = "lance-store")]
mod egress;
#[cfg(feature = "lance-store")]
mod event_row;
#[cfg(feature = "lance-store")]
mod events;
#[cfg(feature = "lance-store")]
mod files;
#[cfg(feature = "lance-store")]
mod index_build_gate;
#[cfg(feature = "lance-store")]
mod local_query_manifest;
#[cfg(feature = "lance-store")]
mod query_engine;
#[cfg(feature = "lance-store")]
mod root_write_lock;
#[cfg(feature = "lance-store")]
mod run_control;
#[cfg(feature = "lance-store")]
mod storyline;
#[cfg(feature = "lance-store")]
#[path = "storyline/model.rs"]
mod storyline_model;

#[cfg(feature = "lance-store")]
pub(crate) use agenticmd_datafusion::AgenticMdDataSource;
#[cfg(feature = "lance-store")]
pub use attempt_registry::{
    unix_now_ms as attempt_registry_now_ms, AttemptRecord, AttemptRecordState, AttemptRegistry,
};
#[cfg(feature = "lance-store")]
pub use catalog::{
    CatalogDataset, CatalogErrorPolicy, CatalogNamespace, CatalogPage, CatalogProjectionStatus,
    CatalogSnapshotOptions, CatalogSourceDescription, CatalogSourceKind, CatalogSourceRevision,
    CatalogSourceStatus, CatalogStorylineKey, CatalogTrajectoryBundle, DatasetCatalogSnapshot,
    DatasetMount, DiscoveredSource, NamespacePath, CATALOG_SOURCES_TABLE,
    CATALOG_TRAJECTORIES_TABLE, DEFAULT_DATASET_NAME, DEFAULT_MAX_EVENT_FALLBACK_BYTES,
    DEFAULT_MAX_EVENT_FALLBACK_ROWS,
};
#[cfg(feature = "lance-store")]
pub(crate) use document_source::{open_document_source, DocumentSourceImpl};
#[cfg(feature = "lance-store")]
pub use egress::{export_source_dirs, export_story_bundle, ExportOutcome};
#[cfg(feature = "lance-store")]
pub use event_row::{event_record_to_event_row, event_row_to_event_record, EventRow};
#[cfg(feature = "lance-store")]
pub(crate) use events::{compact_sealed_event_segment, SealedEventSegment};
#[cfg(feature = "lance-store")]
pub use events::{
    distinct_session_ids_in_run, event_rows_from_batch, maintain as maintain_raw_events,
    raw_event_arrow_schema, EventFactSnapshot, EventLogLayoutStats, EventWriterFence,
    LanceMaintenanceOptions, LanceMaintenanceReport, ObjectStoreManifestWriteMode,
    RawEventDataSource, RawEventLanceAppender, DATAFUSION_EVENTS_TABLE,
};
#[cfg(feature = "lance-store")]
pub(crate) use files::{
    AtifReader, FileTrajectoryDataSource, FileTrajectoryDataSourceOptions,
    FileTrajectoryQueryMetrics,
};
#[cfg(feature = "lance-store")]
pub use files::{FileTrajectoryQueryMetricsSnapshot, SOURCE_FILE_COLUMN};
#[cfg(feature = "lance-store")]
pub(crate) use local_query_manifest::{
    LocalQueryInputFile, LocalQueryManifest, LocalQueryManifestOptions,
};
#[cfg(feature = "lance-store")]
pub use local_query_manifest::{DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES};
#[cfg(feature = "lance-store")]
pub use query_engine::{
    ChronicleQueryEngine, ChronicleQueryExecutionOptions, ExternalTableFormat, ExternalTableSpec,
    QueryBackendInfo, QuerySnapshot, QueryWriteOutcome,
};
#[cfg(feature = "lance-store")]
pub use run_control::{CommitRunOutcome, LeaseAcquireOutcome, RunControlStore};
#[cfg(feature = "lance-store")]
pub(crate) use storyline::StorylineProjectionPublicationOutcome;
#[cfg(feature = "lance-store")]
pub use storyline::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch, ProjectionSourceSnapshot,
    StorylineContentOptions, StorylineContentReadMode, StorylineDataFusionTableNames,
    StorylineDataSource, StorylineDataSourceOptions, StorylineLanceStore,
    StorylineMaintenanceReport, StorylineProjectionLineage, StorylineStreamImportReport,
    StorylineTableKind, StorylineTablePaths, DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE,
    DATAFUSION_TOOL_CALLS_TABLE, DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES,
};
#[cfg(feature = "lance-store")]
pub use storyline_model::{
    reconstruct_storyline, split_storyline, StoryRunRow, StoryStepRow, StoryToolCallRow,
    StorylineTables,
};

#[cfg(feature = "lance-store")]
use std::path::PathBuf;

#[cfg(feature = "lance-store")]
use crate::formats::EventRecord;
#[cfg(feature = "lance-store")]
use crate::layout::{story_lance_event_path, StoryCoords};

/// Producer-defined Storyline sequence. Physical replay order is the immutable
/// Lance append order and does not require a read-before-write counter.
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_SEQ_COL: &str = "seq";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_EVENT_ID_COL: &str = "event_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_TIMESTAMP_COL: &str = "timestamp";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_SOURCE_COL: &str = "source";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_KIND_COL: &str = "kind";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_SESSION_ID_COL: &str = "session_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_AGENT_ID_COL: &str = "agent_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_CALL_ID_COL: &str = "call_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_PARENT_CALL_ID_COL: &str = "parent_call_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_MODEL_COL: &str = "model";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_TRACE_ID_COL: &str = "trace_id";
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_PAYLOAD_JSON_COL: &str = "payload_json";

/// Canonical physical schema for the Lance event log.
#[cfg(feature = "lance-store")]
pub const TRAJECTORY_COLS: &[&str] = &[
    TRAJECTORY_SEQ_COL,
    TRAJECTORY_EVENT_ID_COL,
    TRAJECTORY_TIMESTAMP_COL,
    TRAJECTORY_KIND_COL,
    TRAJECTORY_SOURCE_COL,
    TRAJECTORY_AGENT_ID_COL,
    TRAJECTORY_SESSION_ID_COL,
    TRAJECTORY_CALL_ID_COL,
    TRAJECTORY_TRACE_ID_COL,
    TRAJECTORY_PARENT_CALL_ID_COL,
    TRAJECTORY_MODEL_COL,
    TRAJECTORY_PAYLOAD_JSON_COL,
];

#[cfg(feature = "lance-store")]
fn canonicalize_event(
    session: &StoryCoords,
    mut record: EventRecord,
) -> anyhow::Result<EventRecord> {
    record.validate().map_err(anyhow::Error::from)?;
    let run_id = session
        .root_session_id
        .as_deref()
        .unwrap_or(&session.session_id);
    fill_missing_identity(&mut record.identity.run_id, run_id);
    fill_missing_identity(&mut record.identity.storyline_id, &session.session_id);
    fill_missing_identity(&mut record.session_id, &session.session_id);
    fill_missing_identity(&mut record.agent_id, &session.agent_id);
    record
        .identity
        .producer
        .get_or_insert_with(|| record.source.clone());
    // `event_id` is optional opaque producer/business data. pChronicle neither
    // generates nor checks it and accepts duplicate IDs as appended facts.
    let textual_timestamp_ms = record
        .timestamp
        .as_deref()
        .map(|timestamp| {
            u64::try_from(
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .with_context(|| format!("parse event timestamp '{timestamp}' as RFC3339"))?
                    .timestamp_millis(),
            )
            .context("event timestamp predates Unix epoch")
        })
        .transpose()?;
    match (record.identity.timestamp_unix_ms, textual_timestamp_ms) {
        (Some(canonical), Some(textual)) => anyhow::ensure!(
            canonical == textual,
            "event timestamp conflict: timestamp_unix_ms={canonical}, RFC3339 timestamp={textual}"
        ),
        (None, textual) => {
            record.identity.timestamp_unix_ms =
                Some(textual.unwrap_or_else(attempt_registry_now_ms));
        }
        (Some(_), None) => {}
    }
    Ok(record)
}

#[cfg(feature = "lance-store")]
fn fill_missing_identity(actual: &mut Option<String>, fallback: &str) {
    if actual.is_none() {
        *actual = Some(fallback.to_string());
    }
}

#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOutcome {
    pub accepted_records: usize,
    pub persisted_units: usize,
    pub note: String,
}

#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayOutcome {
    pub records: Vec<EventRecord>,
    pub note: String,
}

#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryStats {
    pub dataset: String,
    pub row_count: usize,
    pub manifest_revision: Option<u64>,
    pub status: String,
    pub note: String,
}

#[cfg(feature = "lance-store")]
#[derive(Debug, Clone, Copy, Default)]
pub struct RawEventLanceStore;

#[cfg(feature = "lance-store")]
impl RawEventLanceStore {
    pub fn display_path(&self, session: &StoryCoords) -> anyhow::Result<String> {
        events::display_path(session)
    }

    pub async fn exists(&self, session: &StoryCoords) -> anyhow::Result<bool> {
        events::exists(session).await
    }

    pub async fn append_events(
        &self,
        session: &StoryCoords,
        records: &[EventRecord],
    ) -> anyhow::Result<AppendOutcome> {
        events::append_events(session, records).await
    }

    pub async fn replay(
        &self,
        session: &StoryCoords,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<ReplayOutcome> {
        events::replay(session, offset, limit).await
    }

    pub async fn read_events(
        &self,
        session: &StoryCoords,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<EventRecord>> {
        Ok(self.replay(session, offset, limit).await?.records)
    }

    pub async fn stats(&self, session: &StoryCoords) -> anyhow::Result<TrajectoryStats> {
        events::stats(session).await
    }

    /// Append a channel-sized batch while preserving the Storyline identity of
    /// each event. Entries sharing a run-level `events.lance` dataset are
    /// committed together.
    pub async fn append_event_batch(
        &self,
        entries: &[(StoryCoords, EventRecord)],
    ) -> anyhow::Result<AppendOutcome> {
        RawEventLanceAppender::default()
            .append_event_batch(entries)
            .await
    }

    pub async fn maintain(
        &self,
        session: &StoryCoords,
        options: &LanceMaintenanceOptions,
    ) -> anyhow::Result<LanceMaintenanceReport> {
        events::maintain(session, options).await
    }

    pub async fn layout_stats(&self, session: &StoryCoords) -> anyhow::Result<EventLogLayoutStats> {
        events::layout_stats(session).await
    }

    /// Read the latest committed page for an append-only follow loop.
    ///
    /// `None` means the run-level dataset has not been created yet. Once it
    /// exists, an empty `records` page means there are currently no rows after
    /// `offset` for this Storyline.
    pub async fn replay_available(
        &self,
        session: &StoryCoords,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<Option<ReplayOutcome>> {
        events::replay_available(session, offset, limit).await
    }
}

#[cfg(feature = "lance-store")]
pub fn raw_event_lance_path(session: &StoryCoords) -> anyhow::Result<PathBuf> {
    story_lance_event_path(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )
}
