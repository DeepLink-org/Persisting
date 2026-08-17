//! ACTF v1.0 — structured agent attempt trajectory format.
//!
//! ACTF stores one benchmark task result, keyed attempts, and one structured
//! agent trajectory per attempt. Steps pair assistant content and tool uses
//! with the environment observations produced by those calls.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{Error, Result};

pub const ACTF_SCHEMA_VERSION: &str = "ACTF_v1.0";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfDocument {
    pub task_id: String,
    pub category: String,
    pub k: u64,
    pub correct: bool,
    pub attempts_tried: u64,
    pub solved_at: Value,
    pub attempts: BTreeMap<String, ActfAttempt>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfAttempt {
    pub correct: bool,
    pub final_answer: Value,
    pub ground_truth: String,
    pub trajectory: ActfTrajectory,
    pub status: String,
    pub score: Value,
    pub error: String,
    pub artifacts: Value,
    pub extra: Value,
    pub analysis_result: Value,
    pub meta: Value,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfTrajectory {
    pub schema_version: String,
    pub steps: Vec<ActfStep>,
    pub started_at: String,
    pub finished_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfStep {
    pub step_id: i64,
    pub assistant_content: ActfAssistantContent,
    pub metric: ActfMetric,
    pub system_prompt: String,
    pub user_content: String,
    pub tools: Vec<ActfToolCall>,
    pub observation: Vec<ActfObservation>,
    pub started_at: String,
    pub finished_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfAssistantContent {
    pub content: String,
    pub reasoning_content: String,
    pub tool_calls: Vec<ActfToolCall>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfMetric {
    pub prompt_tokens_len: Value,
    pub completion_tokens_len: Value,
    pub llm_infer_ms: Value,
    pub env_action_ms: Value,
    pub stop_reason: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfToolCall {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfObservation {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ActfDocument {
    pub const FORMAT_NAME: &'static str = "actf";

    pub fn from_json_str(input: &str) -> Result<Self> {
        let document: Self = serde_json::from_str(input)?;
        document.validate()?;
        Ok(document)
    }

    pub fn to_json_string_pretty(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.task_id.trim().is_empty() {
            return Err(Error::Other("ACTF task_id is required".into()));
        }
        if self.category.trim().is_empty() {
            return Err(Error::Other("ACTF category is required".into()));
        }
        if !(self.solved_at.is_null() || self.solved_at.is_string()) {
            return Err(Error::Other(
                "ACTF solved_at must be a string or null".into(),
            ));
        }
        if self.k == 0 {
            return Err(Error::Other("ACTF k must be positive".into()));
        }
        if self.attempts.is_empty() {
            return Err(Error::Other("ACTF attempts must not be empty".into()));
        }
        if self.attempts_tried != self.attempts.len() as u64 {
            return Err(Error::Other(format!(
                "ACTF attempts_tried={} does not match attempts length {}",
                self.attempts_tried,
                self.attempts.len()
            )));
        }
        if self.attempts_tried > self.k {
            return Err(Error::Other(format!(
                "ACTF attempts_tried={} exceeds k={}",
                self.attempts_tried, self.k
            )));
        }
        for (attempt_id, attempt) in &self.attempts {
            if attempt_id.trim().is_empty() {
                return Err(Error::Other("ACTF attempt id must not be empty".into()));
            }
            if attempt.status.trim().is_empty() {
                return Err(Error::Other(format!(
                    "ACTF attempt '{attempt_id}' status is required"
                )));
            }
            attempt
                .trajectory
                .validate()
                .map_err(|error| Error::Other(format!("ACTF attempt '{attempt_id}': {error}")))?;
        }
        Ok(())
    }
}

impl ActfTrajectory {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != ACTF_SCHEMA_VERSION {
            return Err(Error::Other(format!(
                "unsupported ACTF schema_version '{}'; expected {}",
                self.schema_version, ACTF_SCHEMA_VERSION
            )));
        }
        if self.started_at.trim().is_empty() || self.finished_at.trim().is_empty() {
            return Err(Error::Other(
                "ACTF trajectory started_at and finished_at are required".into(),
            ));
        }
        if self.steps.is_empty() {
            return Err(Error::Other(
                "ACTF trajectory steps must not be empty".into(),
            ));
        }

        let mut previous_step = None;
        for step in &self.steps {
            if step.step_id < 1 {
                return Err(Error::Other(format!(
                    "ACTF step_id must be positive, got {}",
                    step.step_id
                )));
            }
            if previous_step.is_some_and(|previous| step.step_id <= previous) {
                return Err(Error::Other(format!(
                    "ACTF step_id {} is not strictly increasing",
                    step.step_id
                )));
            }
            previous_step = Some(step.step_id);
            if step.started_at.trim().is_empty() || step.finished_at.trim().is_empty() {
                return Err(Error::Other(format!(
                    "ACTF step {} requires started_at and finished_at",
                    step.step_id
                )));
            }
            if step.assistant_content.tool_calls != step.tools {
                return Err(Error::Other(format!(
                    "ACTF step {} assistant_content.tool_calls must equal tools",
                    step.step_id
                )));
            }
            if !(step.metric.prompt_tokens_len.is_null()
                || step.metric.prompt_tokens_len.is_number())
                || !(step.metric.completion_tokens_len.is_null()
                    || step.metric.completion_tokens_len.is_number())
                || !(step.metric.llm_infer_ms.is_null() || step.metric.llm_infer_ms.is_number())
                || !(step.metric.env_action_ms.is_null() || step.metric.env_action_ms.is_number())
            {
                return Err(Error::Other(format!(
                    "ACTF step {} token and latency metrics must be numbers or null",
                    step.step_id
                )));
            }

            let mut step_call_ids = HashSet::new();
            for call in &step.tools {
                if call.kind.trim().is_empty() || call.id.trim().is_empty() {
                    return Err(Error::Other(format!(
                        "ACTF step {} tool calls require type and id",
                        step.step_id
                    )));
                }
                if !step_call_ids.insert(call.id.as_str()) {
                    return Err(Error::Other(format!(
                        "duplicate ACTF tool call id '{}'",
                        call.id
                    )));
                }
            }
            for observation in &step.observation {
                if observation.kind.trim().is_empty() {
                    return Err(Error::Other(format!(
                        "ACTF step {} observation type is required",
                        step.step_id
                    )));
                }
                let referenced_id = observation
                    .extra
                    .get("tool_use_id")
                    .or_else(|| observation.extra.get("id"))
                    .and_then(Value::as_str);
                if let Some(referenced_id) = referenced_id {
                    if !step_call_ids.contains(referenced_id) {
                        return Err(Error::Other(format!(
                            "ACTF step {} observation references unknown tool id '{}'",
                            step.step_id, referenced_id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn parse_actf_document(input: &str) -> Result<ActfDocument> {
    ActfDocument::from_json_str(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> ActfDocument {
        serde_json::from_value(json!({
            "task_id":"task-1",
            "category":"software-engineering",
            "k":1,
            "correct":false,
            "attempts_tried":1,
            "solved_at":null,
            "attempts":{
                "1":{
                    "correct":false,
                    "final_answer":null,
                    "ground_truth":"expected",
                    "trajectory":{
                        "schema_version":"ACTF_v1.0",
                        "steps":[{
                            "step_id":1,
                            "assistant_content":{
                                "content":"",
                                "reasoning_content":"inspect",
                                "tool_calls":[{
                                    "type":"tool_use","id":"call-1","name":"Bash",
                                    "input":{"command":"pwd"}
                                }]
                            },
                            "metric":{
                                "prompt_tokens_len":2,"completion_tokens_len":3,
                                "llm_infer_ms":10.5,"env_action_ms":4.0,"stop_reason":null
                            },
                            "system_prompt":"system","user_content":"task",
                            "tools":[{
                                "type":"tool_use","id":"call-1","name":"Bash",
                                "input":{"command":"pwd"}
                            }],
                            "observation":[{
                                "tool_use_id":"call-1","type":"tool_result",
                                "content":"/app","is_error":false
                            }],
                            "started_at":"2026-01-01 00:00:00+00:00",
                            "finished_at":"2026-01-01 00:00:01+00:00"
                        }],
                        "started_at":"2026-01-01 00:00:00+00:00",
                        "finished_at":"2026-01-01 00:00:01+00:00"
                    },
                    "status":"completed","score":null,"error":"",
                    "artifacts":{},"extra":{},"analysis_result":{},"meta":{}
                }
            }
        }))
        .unwrap()
    }

    #[test]
    fn parses_and_validates_actf_v1() {
        let document = fixture();
        document.validate().unwrap();
        let json = document.to_json_string_pretty().unwrap();
        assert_eq!(ActfDocument::from_json_str(&json).unwrap(), document);
    }

    #[test]
    fn rejects_observation_without_matching_tool() {
        let mut document = fixture();
        document.attempts.get_mut("1").unwrap().trajectory.steps[0].observation[0]
            .extra
            .insert("tool_use_id".into(), Value::String("missing".into()));
        assert!(document
            .validate()
            .unwrap_err()
            .to_string()
            .contains("unknown"));
    }
}
