//! Rebuildable query view over the three ATIF tables.
//!
//! The logical view `atif_trajectory` is:
//!
//! ```sql
//! sessions ⋈ steps  LEFT JOIN  tool_calls
//!   ON sessions.session_id = steps.session_id
//!  AND steps.session_id = tool_calls.session_id
//!  AND steps.step_id = tool_calls.step_id
//! ```
//!
//! One step with N tool calls expands to N rows; a step with no tool calls
//! still appears once with null tool-call columns.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::schema::{tables, SessionRow, StepRow, ToolCallRow};
use crate::store::ChronicleStore;
use crate::Result;

/// Canonical SQL view name for the denormalized ATIF join.
pub const ATIF_TRAJECTORY_VIEW: &str = "atif_trajectory";

/// One row of the `atif_trajectory` view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifViewRow {
    // --- session ---
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub schema_version: String,
    pub agent_name: String,
    pub agent_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,

    // --- step ---
    pub step_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,

    // --- tool_call (nullable) ---
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_extra: Option<Value>,
}

/// In-process join view over a [`ChronicleStore`].
pub struct AtifTrajectoryView<'a> {
    store: &'a dyn ChronicleStore,
}

impl<'a> AtifTrajectoryView<'a> {
    pub fn new(store: &'a dyn ChronicleStore) -> Self {
        Self { store }
    }

    /// Query the join view, optionally filtered by `session_id`.
    pub fn query(&self, session_id: Option<&str>) -> Result<Vec<AtifViewRow>> {
        let sessions = match session_id {
            Some(id) => self
                .store
                .get_session(id)?
                .into_iter()
                .collect::<Vec<_>>(),
            None => self.store.list_sessions()?,
        };

        let mut out = Vec::new();
        for session in sessions {
            out.extend(join_session(self.store, &session)?);
        }
        out.sort_by(|a, b| {
            a.session_id
                .cmp(&b.session_id)
                .then(a.step_id.cmp(&b.step_id))
                .then(a.tool_call_id.cmp(&b.tool_call_id))
        });
        Ok(out)
    }
}

fn join_session(store: &dyn ChronicleStore, session: &SessionRow) -> Result<Vec<AtifViewRow>> {
    let steps = store.list_steps(&session.session_id)?;
    let tool_calls = store.list_tool_calls(&session.session_id)?;
    let mut by_step: std::collections::BTreeMap<i64, Vec<&ToolCallRow>> =
        std::collections::BTreeMap::new();
    for call in &tool_calls {
        by_step.entry(call.step_id).or_default().push(call);
    }

    let mut rows = Vec::new();
    for step in &steps {
        match by_step.get(&step.step_id) {
            Some(calls) if !calls.is_empty() => {
                for call in calls {
                    rows.push(flatten(session, step, Some(call)));
                }
            }
            _ => rows.push(flatten(session, step, None)),
        }
    }
    Ok(rows)
}

fn flatten(session: &SessionRow, step: &StepRow, call: Option<&ToolCallRow>) -> AtifViewRow {
    AtifViewRow {
        session_id: session.session_id.clone(),
        trajectory_id: session.trajectory_id.clone(),
        schema_version: session.schema_version.clone(),
        agent_name: session.agent_name.clone(),
        agent_version: session.agent_version.clone(),
        agent_model_name: session.agent_model_name.clone(),
        notes: session.notes.clone(),
        final_metrics: session.final_metrics.clone(),
        step_id: step.step_id,
        timestamp: step.timestamp.clone(),
        source: step.source.clone(),
        model_name: step.model_name.clone(),
        message: step.message.clone(),
        reasoning_content: step.reasoning_content.clone(),
        observation: step.observation.clone(),
        metrics: step.metrics.clone(),
        llm_call_count: step.llm_call_count,
        is_copied_context: step.is_copied_context,
        tool_call_id: call.map(|c| c.tool_call_id.clone()),
        function_name: call.map(|c| c.function_name.clone()),
        arguments: call.map(|c| c.arguments.clone()),
        tool_call_extra: call.and_then(|c| c.extra.clone()),
    }
}

/// SQL DDL that materializes the same join semantics for DuckDB / SQLite / etc.
///
/// Expects physical tables named `sessions`, `steps`, and `tool_calls`.
pub fn atif_trajectory_sql_ddl() -> String {
    format!(
        "CREATE VIEW IF NOT EXISTS {view} AS\n\
         SELECT\n\
           s.session_id,\n\
           s.trajectory_id,\n\
           s.schema_version,\n\
           s.agent_name,\n\
           s.agent_version,\n\
           s.agent_model_name,\n\
           s.notes,\n\
           s.final_metrics,\n\
           st.step_id,\n\
           st.timestamp,\n\
           st.source,\n\
           st.model_name,\n\
           st.message,\n\
           st.reasoning_content,\n\
           st.observation,\n\
           st.metrics,\n\
           st.llm_call_count,\n\
           st.is_copied_context,\n\
           tc.tool_call_id,\n\
           tc.function_name,\n\
           tc.arguments,\n\
           tc.extra AS tool_call_extra\n\
         FROM {sessions} AS s\n\
         JOIN {steps} AS st\n\
           ON s.session_id = st.session_id\n\
         LEFT JOIN {tool_calls} AS tc\n\
           ON st.session_id = tc.session_id\n\
          AND st.step_id = tc.step_id;\n",
        view = ATIF_TRAJECTORY_VIEW,
        sessions = tables::SESSIONS,
        steps = tables::STEPS,
        tool_calls = tables::TOOL_CALLS,
    )
}
