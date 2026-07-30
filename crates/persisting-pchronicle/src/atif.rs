//! ATIF (Agent Trajectory Interchange Format) document types.
//!
//! Compatible with Harbor RFC 0001 (ATIF-v1.x). This is the **interchange**
//! document shape. Canonical storage uses the normalized tables in [`crate::schema`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Root ATIF trajectory document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifTrajectory {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: AtifAgent,
    pub steps: Vec<AtifStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_trajectories: Option<Vec<AtifTrajectory>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifAgent {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifStep {
    pub step_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Value>,
    /// String or multimodal content-part array.
    pub message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AtifToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<AtifObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifObservation {
    pub results: Vec<Value>,
}

impl AtifTrajectory {
    pub fn from_json_str(s: &str) -> crate::Result<Self> {
        let traj: Self = serde_json::from_str(s)?;
        traj.validate()?;
        Ok(traj)
    }

    pub fn to_json_string_pretty(&self) -> crate::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Effective run-scoped session id used as the table join key.
    ///
    /// Preference: `session_id` → `trajectory_id` → error.
    pub fn effective_session_id(&self) -> crate::Result<&str> {
        if let Some(id) = self.session_id.as_deref().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        if let Some(id) = self.trajectory_id.as_deref().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        Err(crate::Error::InvalidAtif(
            "ATIF trajectory requires session_id or trajectory_id".into(),
        ))
    }

    pub fn validate(&self) -> crate::Result<()> {
        let _ = self.effective_session_id()?;
        if self.agent.name.is_empty() {
            return Err(crate::Error::InvalidAtif("agent.name is required".into()));
        }
        if self.agent.version.is_empty() {
            return Err(crate::Error::InvalidAtif(
                "agent.version is required".into(),
            ));
        }
        let mut seen_steps = std::collections::HashSet::new();
        let mut seen_tools = std::collections::HashSet::new();
        for step in &self.steps {
            if step.step_id < 1 {
                return Err(crate::Error::InvalidAtif(format!(
                    "step_id must start from 1, got {}",
                    step.step_id
                )));
            }
            if !seen_steps.insert(step.step_id) {
                return Err(crate::Error::InvalidAtif(format!(
                    "duplicate step_id {}",
                    step.step_id
                )));
            }
            if let Some(calls) = &step.tool_calls {
                for call in calls {
                    if call.tool_call_id.is_empty() {
                        return Err(crate::Error::InvalidAtif(
                            "tool_call_id must be non-empty".into(),
                        ));
                    }
                    if !seen_tools.insert(call.tool_call_id.clone()) {
                        return Err(crate::Error::InvalidAtif(format!(
                            "duplicate tool_call_id {}",
                            call.tool_call_id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}
