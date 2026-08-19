//! ATIF (Agent Trajectory Interchange Format) document types.
//!
//! Compatible with Harbor RFC 0001 (ATIF-v1.x). This is the **interchange**
//! document shape. Query storage converts it through Storyline into the shared
//! `runs` / `steps` / `tool_calls` schema.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::formats::storyline::FieldPresence;

/// Root ATIF trajectory document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifTrajectory {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub session_id: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub trajectory_id: FieldPresence<String>,
    pub agent: AtifAgent,
    pub steps: Vec<AtifStep>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub notes: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub final_metrics: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub continued_trajectory_ref: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub extra: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub subagent_trajectories: FieldPresence<Vec<AtifTrajectory>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifAgent {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub model_name: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub tool_definitions: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub extra: FieldPresence<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifStep {
    pub step_id: i64,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub timestamp: FieldPresence<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub model_name: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub reasoning_effort: FieldPresence<Value>,
    /// String or multimodal content-part array.
    pub message: Value,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub reasoning_content: FieldPresence<String>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub tool_calls: FieldPresence<Vec<AtifToolCall>>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub observation: FieldPresence<AtifObservation>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub metrics: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub extra: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub llm_call_count: FieldPresence<i64>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub is_copied_context: FieldPresence<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Value,
    /// Inline result. ATIF distinguishes an omitted result from explicit null.
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub result: FieldPresence<Value>,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub extra: FieldPresence<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifObservation {
    pub results: Vec<Value>,
}

impl AtifTrajectory {
    pub fn from_json_str(s: &str) -> crate::InputResult<Self> {
        let traj: Self = serde_json::from_str(s)
            .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
        traj.validate()?;
        Ok(traj)
    }

    /// Effective session partition. Table relationships use `document_id`.
    ///
    /// Preference: `session_id` → `trajectory_id` → error.
    pub fn effective_session_id(&self) -> crate::InputResult<&str> {
        if let Some(id) = self.session_id.value().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        if let Some(id) = self.trajectory_id.value().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        Err(crate::InputIssue::invalid(
            "ATIF trajectory requires session_id or trajectory_id",
        ))
    }

    pub fn validate(&self) -> crate::InputResult<()> {
        let mut trajectory_ids = std::collections::HashSet::new();
        self.validate_inner(false, &mut trajectory_ids)
    }

    fn validate_inner(
        &self,
        embedded: bool,
        trajectory_ids: &mut std::collections::HashSet<String>,
    ) -> crate::InputResult<()> {
        if embedded {
            let trajectory_id = self
                .trajectory_id
                .value()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    crate::InputIssue::invalid("embedded ATIF trajectory requires trajectory_id")
                })?;
            if !trajectory_ids.insert(trajectory_id.clone()) {
                return Err(crate::InputIssue::invalid(format!(
                    "duplicate embedded trajectory_id '{trajectory_id}'"
                )));
            }
        } else {
            let _ = self.effective_session_id()?;
            if let Some(trajectory_id) =
                self.trajectory_id.value().filter(|value| !value.is_empty())
            {
                trajectory_ids.insert(trajectory_id.clone());
            }
        }
        if self.agent.name.is_empty() {
            return Err(crate::InputIssue::invalid("agent.name is required"));
        }
        if self.agent.version.is_empty() {
            return Err(crate::InputIssue::invalid("agent.version is required"));
        }
        let mut seen_steps = std::collections::HashSet::new();
        let mut seen_tools = std::collections::HashSet::new();
        for step in &self.steps {
            if step.step_id < 1 {
                return Err(crate::InputIssue::invalid(format!(
                    "step_id must start from 1, got {}",
                    step.step_id
                )));
            }
            if !seen_steps.insert(step.step_id) {
                return Err(crate::InputIssue::invalid(format!(
                    "duplicate step_id {}",
                    step.step_id
                )));
            }
            if let Some(calls) = step.tool_calls.value() {
                for call in calls {
                    if call.tool_call_id.is_empty() {
                        return Err(crate::InputIssue::invalid("tool_call_id must be non-empty"));
                    }
                    if !seen_tools.insert(call.tool_call_id.clone()) {
                        return Err(crate::InputIssue::invalid(format!(
                            "duplicate tool_call_id {}",
                            call.tool_call_id
                        )));
                    }
                }
            }
        }
        if let Some(children) = self.subagent_trajectories.value() {
            for child in children {
                child.validate_inner(true, trajectory_ids)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_subagents_require_unique_nonempty_trajectory_ids() {
        let duplicate = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "session_id": "shared-run",
            "trajectory_id": "root",
            "agent": {"name": "root", "version": "1"},
            "steps": [],
            "subagent_trajectories": [
                {
                    "schema_version": "ATIF-v1.7",
                    "trajectory_id": "child",
                    "agent": {"name": "first", "version": "1"},
                    "steps": []
                },
                {
                    "schema_version": "ATIF-v1.7",
                    "session_id": "shared-run",
                    "trajectory_id": "child",
                    "agent": {"name": "second", "version": "1"},
                    "steps": []
                }
            ]
        });

        let error = AtifTrajectory::from_json_str(&duplicate.to_string()).unwrap_err();
        assert!(error
            .to_string()
            .contains("duplicate embedded trajectory_id 'child'"));

        let missing = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "session_id": "shared-run",
            "trajectory_id": "root",
            "agent": {"name": "root", "version": "1"},
            "steps": [],
            "subagent_trajectories": [{
                "schema_version": "ATIF-v1.7",
                "agent": {"name": "child", "version": "1"},
                "steps": []
            }]
        });
        let error = AtifTrajectory::from_json_str(&missing.to_string()).unwrap_err();
        assert!(error
            .to_string()
            .contains("embedded ATIF trajectory requires trajectory_id"));
    }
}
