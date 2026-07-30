//! Chronicle storage backends for the three ATIF tables.

mod fs;
mod memory;

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
