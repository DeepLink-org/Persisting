//! Chronicle storage backends: ATIF tables, agenticmd FS, egress, event-row helpers.

mod agenticmd_fs;
mod egress;
mod event_row;
mod events_log;
mod fs;
mod memory;

pub use agenticmd_fs::{
    agenticmd_block_count, append_agenticmd_blocks, encode_agenticmd_block_validated,
    find_block_by_call_id_and_role, parse_agenticmd_document_validated,
    parse_agenticmd_spans_validated, read_agenticmd_blocks_from_file, rewrite_block_range,
    upsert_block_by_call_id,
};
pub use egress::{export_source_dirs, export_story_bundle, parse_engine_records, ExportOutcome};
pub use event_row::{
    event_record_to_event_row, event_row_to_event_record, event_row_to_replay_json, EventRow,
};
pub use events_log::EventLogStore;
pub use fs::FsChronicleStore;
pub use memory::MemoryChronicleStore;

use crate::schema::{SessionRow, StepRow, ToolCallRow};
use crate::Result;

/// Persistence API for normalized ATIF tables.
pub trait ChronicleStore: Send {
    fn upsert_session(&mut self, row: SessionRow) -> Result<()>;
    fn get_session(&self, session_id: &str) -> Result<Option<SessionRow>>;
    fn list_sessions(&self) -> Result<Vec<SessionRow>>;

    fn replace_steps(&mut self, session_id: &str, rows: Vec<StepRow>) -> Result<()>;
    fn list_steps(&self, session_id: &str) -> Result<Vec<StepRow>>;

    fn replace_tool_calls(&mut self, session_id: &str, rows: Vec<ToolCallRow>) -> Result<()>;
    fn list_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCallRow>>;

    fn list_tool_calls_for_step(&self, session_id: &str, step_id: i64) -> Result<Vec<ToolCallRow>> {
        Ok(self
            .list_tool_calls(session_id)?
            .into_iter()
            .filter(|r| r.step_id == step_id)
            .collect())
    }
}
