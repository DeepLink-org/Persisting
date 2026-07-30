//! In-memory chronicle store (tests and local prototyping).

use std::collections::BTreeMap;

use crate::schema::{SessionRow, StepRow, ToolCallRow};
use crate::store::ChronicleStore;
use crate::Result;

#[derive(Debug, Default, Clone)]
pub struct MemoryChronicleStore {
    sessions: BTreeMap<String, SessionRow>,
    steps: BTreeMap<String, Vec<StepRow>>,
    tool_calls: BTreeMap<String, Vec<ToolCallRow>>,
}

impl MemoryChronicleStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ChronicleStore for MemoryChronicleStore {
    fn upsert_session(&mut self, row: SessionRow) -> Result<()> {
        self.sessions.insert(row.session_id.clone(), row);
        Ok(())
    }

    fn get_session(&self, session_id: &str) -> Result<Option<SessionRow>> {
        Ok(self.sessions.get(session_id).cloned())
    }

    fn list_sessions(&self) -> Result<Vec<SessionRow>> {
        Ok(self.sessions.values().cloned().collect())
    }

    fn replace_steps(&mut self, session_id: &str, mut rows: Vec<StepRow>) -> Result<()> {
        for row in &rows {
            if row.session_id != session_id {
                return Err(crate::Error::Other(format!(
                    "step session_id {} does not match {}",
                    row.session_id, session_id
                )));
            }
        }
        rows.sort_by_key(|r| r.step_id);
        self.steps.insert(session_id.to_string(), rows);
        Ok(())
    }

    fn list_steps(&self, session_id: &str) -> Result<Vec<StepRow>> {
        Ok(self.steps.get(session_id).cloned().unwrap_or_default())
    }

    fn replace_tool_calls(&mut self, session_id: &str, mut rows: Vec<ToolCallRow>) -> Result<()> {
        let steps = self.list_steps(session_id)?;
        let step_ids: std::collections::HashSet<i64> = steps.iter().map(|s| s.step_id).collect();
        for row in &rows {
            if row.session_id != session_id {
                return Err(crate::Error::Other(format!(
                    "tool_call session_id {} does not match {}",
                    row.session_id, session_id
                )));
            }
            if !step_ids.contains(&row.step_id) {
                return Err(crate::Error::OrphanToolCall {
                    session_id: session_id.to_string(),
                    step_id: row.step_id,
                    tool_call_id: row.tool_call_id.clone(),
                });
            }
        }
        rows.sort_by(|a, b| a.step_id.cmp(&b.step_id).then(a.tool_call_id.cmp(&b.tool_call_id)));
        self.tool_calls.insert(session_id.to_string(), rows);
        Ok(())
    }

    fn list_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCallRow>> {
        Ok(self.tool_calls.get(session_id).cloned().unwrap_or_default())
    }
}
