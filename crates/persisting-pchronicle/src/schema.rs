//! Normalized ATIF tables.
//!
//! | Table | Primary key | Foreign key |
//! |---|---|---|
//! | [`SessionRow`] | `session_id` | — |
//! | [`StepRow`] | (`session_id`, `step_id`) | → sessions |
//! | [`ToolCallRow`] | (`session_id`, `tool_call_id`) | → steps via (`session_id`, `step_id`) |

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `sessions` table — one row per ATIF trajectory / agent session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub schema_version: String,
    pub agent_name: String,
    pub agent_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_tool_definitions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    /// Embedded subagent trajectories kept as JSON array (optional; not flattened).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_trajectories: Option<Value>,
}

/// `steps` table — one row per ATIF step.
///
/// Tool calls are stored in [`ToolCallRow`]; `observation` stays on the step
/// because results correlate via `source_call_id` inside the JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRow {
    pub session_id: String,
    pub step_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Value>,
    pub message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
}

/// `tool_calls` table — one row per tool call, linked to its owning step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRow {
    pub session_id: String,
    pub step_id: i64,
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// Logical table names used by the filesystem layout and SQL view DDL.
pub mod tables {
    pub const SESSIONS: &str = "sessions";
    pub const STEPS: &str = "steps";
    pub const TOOL_CALLS: &str = "tool_calls";
}
