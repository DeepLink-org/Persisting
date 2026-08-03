//! Chronicle storage backends: ATIF tables, agenticmd FS, egress, event-row helpers.

mod agenticmd_fs;
mod atif_datafusion;
mod egress;
mod event_row;
mod lance;
mod lance_rows;
pub(crate) mod markdown;
mod memory;
mod query_engine;
mod run_control;
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
pub use atif_datafusion::{load_atif_trajectories, AtifDataSource, AtifDataSourceOptions};
pub use egress::{export_source_dirs, export_story_bundle, parse_engine_records, ExportOutcome};
pub use event_row::{
    event_record_to_event_row, event_row_to_event_record, event_row_to_replay_json, EventRow,
};
pub use lance::{distinct_session_ids_in_run, overwrite_session_events, overwrite_session_lines};
pub use lance_rows::{
    event_row_from_batch, event_rows_from_batch, event_rows_to_batch, trajectory_arrow_schema,
};
pub use memory::MemoryChronicleStore;
pub use query_engine::{ChronicleQueryBackend, ChronicleQueryEngine};
pub use run_control::{CommitRunOutcome, LeaseAcquireOutcome, RunControlStore};
pub use storyline_datafusion::{
    StorylineDataFusionTableNames, StorylineDataSource, StorylineDataSourceOptions,
    StorylineTableKind, StorylineTableProvider, DATAFUSION_RUNS_TABLE, DATAFUSION_STEPS_TABLE,
    DATAFUSION_TOOL_CALLS_TABLE,
};
pub use storyline_lance::{LanceStorylineStore, StorylineTablePaths};
pub use storyline_lance_rows::{
    story_runs_arrow_schema, story_runs_from_batch, story_runs_to_batch, story_steps_arrow_schema,
    story_steps_from_batch, story_steps_to_batch, story_tool_calls_arrow_schema,
    story_tool_calls_from_batch, story_tool_calls_to_batch,
};

use anyhow::Context;
use async_trait::async_trait;
use std::path::PathBuf;

use crate::schema::{SessionRow, StepRow, ToolCallRow};
use crate::{story_lance_event_path, EventRecord, Result as ChronicleResult, StoryCoords};

pub const TRAJECTORY_SEQ_COL: &str = "seq";
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

/// Stable physical schema for the canonical Lance event log.
pub const TRAJECTORY_V1_COLS: &[&str] = &[
    TRAJECTORY_SEQ_COL,
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

/// Physical representations owned by pChronicle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageKind {
    Lance,
    AgenticMd,
}

/// Story coordinates accepted by every pChronicle physical backend.
pub type TrajectorySession = StoryCoords;

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

/// Decode the current RPC/engine RON transport into typed events.
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

/// Encode typed records for the current RPC/engine RON transport.
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
    fn kind(&self) -> StorageKind;
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

    /// Typed append API for callers that do not operate on the internal RON transport.
    async fn append_events(
        &self,
        session: &TrajectorySession,
        records: &[EventRecord],
    ) -> anyhow::Result<AppendOutcome> {
        self.append(session, &encode_event_lines(records)?).await
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
pub struct LanceEventStore;

#[derive(Debug, Clone, Copy, Default)]
pub struct AgenticMdStore;

pub fn structured_store(kind: StorageKind) -> Box<dyn StructuredStore> {
    match kind {
        StorageKind::Lance => Box::new(LanceEventStore),
        StorageKind::AgenticMd => Box::new(AgenticMdStore),
    }
}

pub fn session_lance_path(session: &TrajectorySession) -> anyhow::Result<PathBuf> {
    story_lance_event_path(
        &session.storage,
        &session.agent_id,
        &session.session_id,
        session.root_session_id.as_deref(),
    )
}

#[async_trait]
impl StructuredStore for LanceEventStore {
    fn kind(&self) -> StorageKind {
        StorageKind::Lance
    }

    fn display_path(&self, session: &TrajectorySession) -> anyhow::Result<String> {
        lance::display_path(session)
    }

    async fn exists(&self, session: &TrajectorySession) -> anyhow::Result<bool> {
        lance::exists(session).await
    }

    async fn append(
        &self,
        session: &TrajectorySession,
        records_ron: &[String],
    ) -> anyhow::Result<AppendOutcome> {
        lance::append(session, records_ron).await
    }

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<ReplayOutcome> {
        lance::replay(session, offset, limit).await
    }

    async fn stats(&self, session: &TrajectorySession) -> anyhow::Result<TrajectoryStats> {
        lance::stats(session).await
    }
}

#[async_trait]
impl StructuredStore for AgenticMdStore {
    fn kind(&self) -> StorageKind {
        StorageKind::AgenticMd
    }

    fn display_path(&self, session: &TrajectorySession) -> anyhow::Result<String> {
        markdown::display_path(session)
    }

    async fn exists(&self, session: &TrajectorySession) -> anyhow::Result<bool> {
        markdown::exists(session)
    }

    async fn append(
        &self,
        session: &TrajectorySession,
        records_ron: &[String],
    ) -> anyhow::Result<AppendOutcome> {
        markdown::append(session, records_ron)
    }

    async fn replay(
        &self,
        session: &TrajectorySession,
        offset: usize,
        limit: Option<usize>,
    ) -> anyhow::Result<ReplayOutcome> {
        markdown::replay(session, offset, limit)
    }

    async fn stats(&self, session: &TrajectorySession) -> anyhow::Result<TrajectoryStats> {
        markdown::stats(session)
    }
}

/// Persistence API for the rebuildable, normalized ATIF query tables.
///
/// This is distinct from [`StructuredStore`]: the latter owns the canonical
/// event stream and physical trajectory projections, while this trait exposes
/// relational views derived from trajectory documents.
pub trait NormalizedStore: Send {
    fn upsert_session(&mut self, row: SessionRow) -> ChronicleResult<()>;
    fn get_session(&self, session_id: &str) -> ChronicleResult<Option<SessionRow>>;
    fn list_sessions(&self) -> ChronicleResult<Vec<SessionRow>>;

    fn replace_steps(&mut self, session_id: &str, rows: Vec<StepRow>) -> ChronicleResult<()>;
    fn list_steps(&self, session_id: &str) -> ChronicleResult<Vec<StepRow>>;

    fn replace_tool_calls(
        &mut self,
        session_id: &str,
        rows: Vec<ToolCallRow>,
    ) -> ChronicleResult<()>;
    fn list_tool_calls(&self, session_id: &str) -> ChronicleResult<Vec<ToolCallRow>>;

    /// Atomically replace all normalized rows for one trajectory when supported.
    fn replace_trajectory(&mut self, split: crate::ingest::SplitTables) -> ChronicleResult<()> {
        let session_id = split.session.session_id.clone();
        self.upsert_session(split.session)?;
        self.replace_steps(&session_id, split.steps)?;
        self.replace_tool_calls(&session_id, split.tool_calls)
    }

    fn list_tool_calls_for_step(
        &self,
        session_id: &str,
        step_id: i64,
    ) -> ChronicleResult<Vec<ToolCallRow>> {
        Ok(self
            .list_tool_calls(session_id)?
            .into_iter()
            .filter(|r| r.step_id == step_id)
            .collect())
    }
}
