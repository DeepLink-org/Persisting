//! ATIF (Agent Trajectory Interchange Format) document types.
//!
//! Compatible with Harbor RFC 0001 (ATIF-v1.x). This is the **interchange**
//! document shape. Query storage converts it through Storyline into the shared
//! `runs` / `steps` / `tool_calls` schema.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    #[serde(default, flatten, skip_serializing_if = "Map::is_empty")]
    pub unknown: Map<String, Value>,
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
    #[serde(default, flatten, skip_serializing_if = "Map::is_empty")]
    pub unknown: Map<String, Value>,
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
    #[serde(default, flatten, skip_serializing_if = "Map::is_empty")]
    pub unknown: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, flatten, skip_serializing_if = "Map::is_empty")]
    pub unknown: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifObservation {
    pub results: Vec<Value>,
    #[serde(default, flatten, skip_serializing_if = "Map::is_empty")]
    pub unknown: Map<String, Value>,
}

impl AtifTrajectory {
    #[cfg(test)]
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
        if let Some(id) = self.session_id.as_ref().filter(|s| !s.is_empty()) {
            return Ok(id);
        }
        if let Some(id) = self.trajectory_id.as_ref().filter(|s| !s.is_empty()) {
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
                .as_ref()
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
            if let Some(trajectory_id) = self
                .trajectory_id
                .as_ref()
                .filter(|value| !value.is_empty())
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
            if let Some(calls) = step.tool_calls.as_ref() {
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
        if let Some(children) = self.subagent_trajectories.as_ref() {
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
    use proptest::prelude::*;

    fn token_strategy() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z0-9._:-]{1,20}").unwrap()
    }

    fn valid_trajectory_strategy() -> impl Strategy<Value = AtifTrajectory> {
        proptest::collection::vec(1i64..10_000, 0..8).prop_map(|mut ids| {
            ids.sort_unstable();
            ids.dedup();
            let steps = ids
                .iter()
                .map(|step_id| AtifStep {
                    step_id: *step_id,
                    timestamp: None,
                    source: "agent".into(),
                    model_name: None,
                    reasoning_effort: None,
                    message: Value::String("message".into()),
                    reasoning_content: None,
                    tool_calls: Some(vec![AtifToolCall {
                        tool_call_id: format!("call-{step_id}"),
                        function_name: "tool".into(),
                        arguments: serde_json::json!({"ok": true}),
                        result: None,
                        extra: None,
                        unknown: Map::new(),
                    }]),
                    observation: None,
                    metrics: None,
                    extra: None,
                    llm_call_count: None,
                    is_copied_context: None,
                    unknown: Map::new(),
                })
                .collect();
            AtifTrajectory {
                schema_version: "ATIF-v1.7".into(),
                session_id: Some("session".into()),
                trajectory_id: Some("trajectory".into()),
                agent: AtifAgent {
                    name: "agent".into(),
                    version: "1".into(),
                    model_name: None,
                    tool_definitions: None,
                    extra: None,
                    unknown: Map::new(),
                },
                steps,
                notes: None,
                final_metrics: None,
                continued_trajectory_ref: None,
                extra: None,
                subagent_trajectories: None,
                unknown: Map::new(),
            }
        })
    }

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
        assert!(
            error
                .to_string()
                .contains("duplicate embedded trajectory_id 'child'")
        );

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
        assert!(
            error
                .to_string()
                .contains("embedded ATIF trajectory requires trajectory_id")
        );
    }

    proptest! {
        #[test]
        fn valid_trajectories_validate_and_roundtrip(
            trajectory in valid_trajectory_strategy(),
        ) {
            trajectory.validate().unwrap();
            let encoded = serde_json::to_value(&trajectory).unwrap();
            prop_assert_eq!(serde_json::from_value::<AtifTrajectory>(encoded).unwrap(), trajectory);
        }

        #[test]
        fn effective_session_id_prefers_session_then_trajectory(
            session in prop::option::of(token_strategy()),
            trajectory_id in prop::option::of(token_strategy()),
        ) {
            let trajectory = AtifTrajectory {
                schema_version: "ATIF-v1.7".into(),
                session_id: session.clone(),
                trajectory_id: trajectory_id.clone(),
                agent: AtifAgent {
                    name: "agent".into(), version: "1".into(), model_name: None,
                    tool_definitions: None, extra: None, unknown: Map::new(),
                },
                steps: Vec::new(), notes: None, final_metrics: None,
                continued_trajectory_ref: None, extra: None,
                subagent_trajectories: None, unknown: Map::new(),
            };
            let expected = session.as_deref().filter(|id| !id.is_empty())
                .or_else(|| trajectory_id.as_deref().filter(|id| !id.is_empty()));
            prop_assert_eq!(trajectory.effective_session_id().ok(), expected);
        }

        #[test]
        fn invalid_step_ids_are_rejected(
            invalid_step_id in prop_oneof![Just(0i64), (-10i64..0)],
        ) {
            let trajectory = AtifTrajectory {
                schema_version: "ATIF-v1.7".into(),
                session_id: Some("session".into()),
                trajectory_id: None,
                agent: AtifAgent {
                    name: "agent".into(), version: "1".into(), model_name: None,
                    tool_definitions: None, extra: None, unknown: Map::new(),
                },
                steps: vec![AtifStep {
                    step_id: invalid_step_id, timestamp: None, source: "agent".into(),
                    model_name: None, reasoning_effort: None, message: Value::Null,
                    reasoning_content: None, tool_calls: None, observation: None,
                    metrics: None, extra: None, llm_call_count: None,
                    is_copied_context: None, unknown: Map::new(),
                }],
                notes: None, final_metrics: None, continued_trajectory_ref: None,
                extra: None, subagent_trajectories: None, unknown: Map::new(),
            };
            prop_assert!(trajectory.validate().is_err());
        }
    }
}
