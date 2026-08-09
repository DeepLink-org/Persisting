//! pChronicle storage, separated by logical data model.
//!
//! - `raw_event_lance`: canonical append/replay storage for `EventRecord`.
//! - `storyline_lance`: normalized three-table projection for `StorylineDocument`.
//! - `search`: document retrieval storage lives outside this module.

mod agenticmd_fs;
mod atif_datafusion;
mod attempt_registry;
pub(crate) mod dataset_write_lock;
mod egress;
mod event_row;
mod file_trajectory_datafusion;
mod index_build_gate;
mod local_query_manifest;
mod query_engine;
mod raw_event_datafusion;
mod raw_event_lance;
mod raw_event_lance_rows;
mod raw_event_manifest;
mod root_write_lock;
mod run_control;
mod storyline_content;
mod storyline_datafusion;
mod storyline_lance;
mod storyline_lance_rows;

pub use agenticmd_fs::{
    agenticmd_block_count, agenticmd_replay_json_lines, agenticmd_structural_issues,
    append_agenticmd_blocks, count_agenticmd_role, encode_agenticmd_block_validated,
    find_block_by_call_id_and_role, index_agenticmd_path, list_agenticmd_paths,
    parse_agenticmd_document_validated, parse_agenticmd_spans_validated,
    read_agenticmd_blocks_from_file, rewrite_agenticmd_preamble, rewrite_block_range,
    upsert_block_by_call_id, write_agenticmd_document, AgenticmdFileIndex,
};
pub use atif_datafusion::{
    load_atif_trajectories, AtifDataSource, AtifDataSourceOptions, AtifReader,
};
pub use attempt_registry::{
    unix_now_ms as attempt_registry_now_ms, AttemptRecord, AttemptRecordState, AttemptRegistry,
    ATTEMPT_RECORD_SCHEMA_VERSION,
};
pub use egress::{export_source_dirs, export_story_bundle, validate_event_lines, ExportOutcome};
pub use event_row::{
    event_record_to_event_row, event_row_to_event_record, event_row_to_replay_json, EventRow,
};
pub use file_trajectory_datafusion::{
    FileTrajectoryDataSource, FileTrajectoryDataSourceOptions, FileTrajectoryFormat,
    FileTrajectoryQueryMetrics, FileTrajectoryQueryMetricsSnapshot, DEFAULT_LOCAL_QUERY_BATCH_SIZE,
    DEFAULT_LOCAL_QUERY_CACHE_BYTES, DEFAULT_LOCAL_QUERY_CACHE_FILES,
    DEFAULT_LOCAL_QUERY_MAX_FILE_BYTES, SOURCE_FILE_COLUMN,
};
pub use local_query_manifest::{
    detect_local_query_format, detect_local_query_manifest, LocalQueryInputFile,
    LocalQueryManifest, LocalQueryManifestOptions, DEFAULT_MAX_LOCAL_QUERY_DETECTION_BYTES,
    DEFAULT_MAX_LOCAL_QUERY_ENTRIES, DEFAULT_MAX_LOCAL_QUERY_FILES,
};
pub use query_engine::{
    ChronicleQueryBackend, ChronicleQueryEngine, ChronicleQueryExecutionOptions,
    ExternalTableFormat, ExternalTableSpec,
};
pub use raw_event_datafusion::{
    RawEventDataSource, RawEventDataSourceOptions, RawEventTableProvider, DATAFUSION_EVENTS_TABLE,
};
pub use raw_event_lance::{
    distinct_session_ids_in_run, EventLogLayoutStats, EventWriterFence, LanceMaintenanceOptions,
    LanceMaintenanceReport, RawEventLanceAppender,
};
pub use raw_event_lance_rows::{
    event_row_from_batch, event_rows_from_batch, event_rows_to_batch, raw_event_arrow_schema,
};
pub use run_control::{CommitRunOutcome, LeaseAcquireOutcome, RunControlStore};
pub use storyline_content::{
    StorylineContentOptions, DEFAULT_CONTENT_OFFLOAD_THRESHOLD, DEFAULT_CONTENT_PREVIEW_BYTES,
};
pub use storyline_datafusion::{
    StorylineContentReadMode, StorylineDataFusionTableNames, StorylineDataSource,
    StorylineDataSourceOptions, StorylineTableKind, StorylineTableProvider, DATAFUSION_RUNS_TABLE,
    DATAFUSION_STEPS_TABLE, DATAFUSION_TOOL_CALLS_TABLE,
};
pub use storyline_lance::{
    StorylineLanceStore, StorylineMaintenanceReport, StorylineStreamImportReport,
    StorylineTablePaths,
};
pub use storyline_lance_rows::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};

use anyhow::Context;
use async_trait::async_trait;
use std::path::PathBuf;

use crate::{story_lance_event_path, EventRecord, StoryCoords};

/// Producer-defined Storyline sequence. Physical replay order is the immutable
/// Lance append order and does not require a read-before-write counter.
pub const TRAJECTORY_SEQ_COL: &str = "seq";
pub const TRAJECTORY_EVENT_ID_COL: &str = "event_id";
pub const TRAJECTORY_TIMESTAMP_COL: &str = "timestamp";
pub const TRAJECTORY_SOURCE_COL: &str = "source";
pub const TRAJECTORY_KIND_COL: &str = "kind";
pub const TRAJECTORY_SESSION_ID_COL: &str = "session_id";
pub const TRAJECTORY_AGENT_ID_COL: &str = "agent_id";
pub const TRAJECTORY_CALL_ID_COL: &str = "call_id";
pub const TRAJECTORY_PARENT_CALL_ID_COL: &str = "parent_call_id";
pub const TRAJECTORY_MODEL_COL: &str = "model";
pub const TRAJECTORY_TRACE_ID_COL: &str = "trace_id";
pub const TRAJECTORY_PAYLOAD_JSON_COL: &str = "payload_json";

/// Canonical physical schema for the Lance event log.
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

/// Story coordinates accepted by every pChronicle physical backend.
pub type TrajectorySession = StoryCoords;

fn canonicalize_event(session: &TrajectorySession, mut record: EventRecord) -> EventRecord {
    let run_id = session
        .root_session_id
        .as_deref()
        .unwrap_or(&session.session_id);
    record
        .identity
        .run_id
        .get_or_insert_with(|| run_id.to_string());
    record
        .identity
        .storyline_id
        .get_or_insert_with(|| session.session_id.clone());
    record
        .identity
        .producer
        .get_or_insert_with(|| record.source.clone());
    record
        .agent_id
        .get_or_insert_with(|| session.agent_id.clone());
    // `event_id` is optional opaque producer/business data. pChronicle neither
    // generates nor checks it and accepts duplicate IDs as appended facts.
    record
        .identity
        .timestamp_unix_ms
        .get_or_insert_with(attempt_registry_now_ms);
    record
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOutcome {
    pub accepted_records: usize,
    pub persisted_units: usize,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub records: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrajectoryStats {
    pub dataset: String,
    pub row_count: usize,
    pub manifest_version: Option<u64>,
    pub status: String,
    pub note: String,
}

/// Decode newline-delimited RON event values into typed events.
pub fn decode_event_lines(lines: &[String]) -> anyhow::Result<Vec<EventRecord>> {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let value: serde_json::Value = ron::from_str(line.trim())
                .with_context(|| format!("decode record[{index}] RON"))?;
            serde_json::from_value(value)
                .with_context(|| format!("decode record[{index}] as EventRecord"))
        })
        .collect()
}

/// Encode typed records as newline-delimited RON event values.
pub fn encode_event_lines(records: &[EventRecord]) -> anyhow::Result<Vec<String>> {
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let value = serde_json::to_value(record)
                .with_context(|| format!("serialize record[{index}]"))?;
            ron::to_string(&value).with_context(|| format!("encode record[{index}] RON"))
        })
        .collect()
}

/// Unified async API for canonical event storage and rebuildable projections.
#[async_trait]
pub trait StructuredStore: Send + Sync {
    fn display_path(&self, session: &TrajectorySession) -> anyhow::Result<String>;
    async fn exists(&self, session: &TrajectorySession) -> anyhow::Result<bool>;
    async fn append(
        &self,
        session: &TrajectorySession,
        records_ron: &[String],
    ) -> anyhow::Result<AppendOutcome>;
    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<ReplayOutcome>;
    async fn stats(&self, session: &TrajectorySession) -> anyhow::Result<TrajectoryStats>;

    /// Typed append API for callers that already hold canonical events.
    async fn append_events(
        &self,
        session: &TrajectorySession,
        records: &[EventRecord],
    ) -> anyhow::Result<AppendOutcome> {
        let canonical = records
            .iter()
            .cloned()
            .map(|record| canonicalize_event(session, record))
            .collect::<Vec<_>>();
        self.append(session, &encode_event_lines(&canonical)?).await
    }

    /// Typed read API for new callers.
    async fn read_events(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<EventRecord>> {
        let replay = self.replay(session, offset, limit).await?;
        replay
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                serde_json::from_str(record)
                    .with_context(|| format!("decode replay record[{index}] as EventRecord"))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RawEventLanceStore;

impl RawEventLanceStore {
    /// Append a channel-sized batch while preserving the Storyline identity of
    /// each event. Entries sharing a run-level `events.lance` dataset are
    /// committed together.
    pub async fn append_event_batch(
        &self,
        entries: &[(TrajectorySession, EventRecord)],
    ) -> anyhow::Result<AppendOutcome> {
        RawEventLanceAppender::default()
            .append_event_batch(entries)
            .await
    }

    pub async fn maintain(
        &self,
        session: &TrajectorySession,
        options: &LanceMaintenanceOptions,
    ) -> anyhow::Result<LanceMaintenanceReport> {
        raw_event_lance::maintain(session, options).await
    }

    pub async fn layout_stats(
        &self,
        session: &TrajectorySession,
    ) -> anyhow::Result<EventLogLayoutStats> {
        raw_event_lance::layout_stats(session).await
    }

    /// Read the latest committed page for an append-only follow loop.
    ///
    /// `None` means the run-level dataset has not been created yet. Once it
    /// exists, an empty `records` page means there are currently no rows after
    /// `offset` for this Storyline.
    pub async fn replay_available(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<Option<ReplayOutcome>> {
        raw_event_lance::replay_available(session, offset, limit).await
    }
}

pub fn raw_event_lance_path(session: &TrajectorySession) -> anyhow::Result<PathBuf> {
    story_lance_event_path(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )
}

#[async_trait]
impl StructuredStore for RawEventLanceStore {
    fn display_path(&self, session: &TrajectorySession) -> anyhow::Result<String> {
        raw_event_lance::display_path(session)
    }

    async fn exists(&self, session: &TrajectorySession) -> anyhow::Result<bool> {
        raw_event_lance::exists(session).await
    }

    async fn append(
        &self,
        session: &TrajectorySession,
        records_ron: &[String],
    ) -> anyhow::Result<AppendOutcome> {
        raw_event_lance::append(session, records_ron).await
    }

    async fn append_events(
        &self,
        session: &TrajectorySession,
        records: &[EventRecord],
    ) -> anyhow::Result<AppendOutcome> {
        raw_event_lance::append_events(session, records).await
    }

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<ReplayOutcome> {
        raw_event_lance::replay(session, offset, limit).await
    }

    async fn stats(&self, session: &TrajectorySession) -> anyhow::Result<TrajectoryStats> {
        raw_event_lance::stats(session).await
    }
}
