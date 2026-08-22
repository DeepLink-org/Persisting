//! ACTF v1.0 — structured agent attempt trajectory format.
//!
//! ACTF stores one benchmark task result, keyed attempts, and one structured
//! agent trajectory per attempt. Steps pair assistant content and tool uses
//! with the environment observations produced by those calls.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::{InputIssue, InputResult};

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
    #[serde(default)]
    pub final_answer: Value,
    #[serde(default)]
    pub ground_truth: Value,
    pub trajectory: ActfTrajectory,
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub status: String,
    #[serde(default)]
    pub score: Value,
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub error: String,
    #[serde(default)]
    pub artifacts: Value,
    #[serde(default)]
    pub extra: Value,
    #[serde(default)]
    pub analysis_result: Value,
    #[serde(default)]
    pub meta: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub max_score: Value,
    #[serde(flatten)]
    pub extensions: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActfTrajectory {
    pub schema_version: String,
    pub steps: Vec<ActfStep>,
    pub started_at: String,
    pub finished_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ActfTrajectory {
    pub fn from_event_log(events: Vec<Value>) -> Self {
        let started_at = events
            .iter()
            .find_map(|event| event.get("timestamp").and_then(Value::as_str))
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();
        let finished_at = events
            .iter()
            .rev()
            .find_map(|event| event.get("timestamp").and_then(Value::as_str))
            .unwrap_or(started_at.as_str())
            .to_string();
        Self {
            schema_version: ACTF_SCHEMA_VERSION.into(),
            steps: Vec::new(),
            started_at,
            finished_at,
            events,
            extra: Map::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ActfTrajectoryWire {
    Events(Vec<Value>),
    Canonical {
        schema_version: String,
        steps: Vec<ActfStep>,
        started_at: String,
        finished_at: String,
        #[serde(default)]
        events: Vec<Value>,
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

impl<'de> Deserialize<'de> for ActfTrajectory {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ActfTrajectoryWire::deserialize(deserializer)? {
            ActfTrajectoryWire::Events(events) => Ok(Self::from_event_log(events)),
            ActfTrajectoryWire::Canonical {
                schema_version,
                steps,
                started_at,
                finished_at,
                events,
                extra,
            } => Ok(Self {
                schema_version,
                steps,
                started_at,
                finished_at,
                events,
                extra,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfStep {
    pub step_id: i64,
    pub assistant_content: ActfAssistantContent,
    pub metric: ActfMetric,
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub system_prompt: String,
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub user_content: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tools: Vec<ActfToolCall>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub observation: Vec<ActfObservation>,
    pub started_at: String,
    pub finished_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ActfStep {
    pub fn effective_tools(&self) -> &[ActfToolCall] {
        if self.tools.is_empty() {
            &self.assistant_content.tool_calls
        } else {
            &self.tools
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfAssistantContent {
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub content: String,
    #[serde(default, deserialize_with = "null_as_empty_string")]
    pub reasoning_content: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub tool_calls: Vec<ActfToolCall>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfMetric {
    #[serde(default)]
    pub prompt_tokens_len: Value,
    #[serde(default)]
    pub completion_tokens_len: Value,
    #[serde(default)]
    pub llm_infer_ms: Value,
    #[serde(default)]
    pub env_action_ms: Value,
    #[serde(default)]
    pub stop_reason: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfToolCall {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_as_empty_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "null_as_empty_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ActfToolCall {
    pub fn effective_id(&self, step_id: i64, index: usize) -> String {
        if self.id.trim().is_empty() {
            format!("step-{step_id}-tool-{index}")
        } else {
            self.id.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActfObservation {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "null_as_empty_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub kind: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn null_as_empty_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_as_default<'de, T, D>(deserializer: D) -> std::result::Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl ActfDocument {
    #[cfg(any(test, feature = "lance-store"))]
    pub fn from_json_str(input: &str) -> InputResult<Self> {
        let document: Self =
            serde_json::from_str(input).map_err(|error| InputIssue::invalid(error.to_string()))?;
        document.validate()?;
        Ok(document)
    }

    #[cfg(test)]
    pub fn to_json_string_pretty(&self) -> InputResult<String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| InputIssue::invalid(error.to_string()))
    }

    pub fn validate(&self) -> InputResult<()> {
        if self.task_id.trim().is_empty() {
            return Err(InputIssue::invalid("ACTF task_id is required"));
        }
        if self.category.trim().is_empty() {
            return Err(InputIssue::invalid("ACTF category is required"));
        }
        if !(self.solved_at.is_null() || self.solved_at.is_string()) {
            return Err(InputIssue::invalid(
                "ACTF solved_at must be a string or null",
            ));
        }
        if self.k == 0 {
            return Err(InputIssue::invalid("ACTF k must be positive"));
        }
        if self.attempts.is_empty() {
            return Err(InputIssue::invalid("ACTF attempts must not be empty"));
        }
        if self.attempts_tried != self.attempts.len() as u64 {
            return Err(InputIssue::invalid(format!(
                "ACTF attempts_tried={} does not match attempts length {}",
                self.attempts_tried,
                self.attempts.len()
            )));
        }
        if self.attempts_tried > self.k {
            return Err(InputIssue::invalid(format!(
                "ACTF attempts_tried={} exceeds k={}",
                self.attempts_tried, self.k
            )));
        }
        for (attempt_id, attempt) in &self.attempts {
            if attempt_id.trim().is_empty() {
                return Err(InputIssue::invalid("ACTF attempt id must not be empty"));
            }
            attempt
                .trajectory
                .validate()
                .map_err(|error| error.at(format!("attempts.{attempt_id}.trajectory")))?;
        }
        Ok(())
    }
}

impl ActfTrajectory {
    pub fn validate(&self) -> InputResult<()> {
        if self.schema_version != ACTF_SCHEMA_VERSION {
            return Err(InputIssue::unsupported(format!(
                "unsupported ACTF schema_version '{}'; expected {}",
                self.schema_version, ACTF_SCHEMA_VERSION
            )));
        }
        if self.started_at.trim().is_empty() || self.finished_at.trim().is_empty() {
            return Err(InputIssue::invalid(
                "ACTF trajectory started_at and finished_at are required",
            ));
        }
        if self.steps.is_empty() && self.events.is_empty() {
            return Err(InputIssue::invalid(
                "ACTF trajectory steps must not be empty",
            ));
        }

        let mut previous_step = None;
        for step in &self.steps {
            if step.step_id < 1 {
                return Err(InputIssue::invalid(format!(
                    "ACTF step_id must be positive, got {}",
                    step.step_id
                )));
            }
            if previous_step.is_some_and(|previous| step.step_id <= previous) {
                return Err(InputIssue::invalid(format!(
                    "ACTF step_id {} is not strictly increasing",
                    step.step_id
                )));
            }
            previous_step = Some(step.step_id);
            if step.started_at.trim().is_empty() || step.finished_at.trim().is_empty() {
                return Err(InputIssue::invalid(format!(
                    "ACTF step {} requires started_at and finished_at",
                    step.step_id
                )));
            }
            if !step.tools.is_empty()
                && !step.assistant_content.tool_calls.is_empty()
                && step.assistant_content.tool_calls != step.tools
            {
                return Err(InputIssue::invalid(format!(
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
                return Err(InputIssue::invalid(format!(
                    "ACTF step {} token and latency metrics must be numbers or null",
                    step.step_id
                )));
            }

            let mut step_call_ids = HashSet::new();
            for (call_index, call) in step.effective_tools().iter().enumerate() {
                let call_id = call.effective_id(step.step_id, call_index);
                if !step_call_ids.insert(call_id) {
                    return Err(InputIssue::invalid(format!(
                        "duplicate ACTF tool call id '{}'",
                        call.effective_id(step.step_id, call_index)
                    )));
                }
            }
            for observation in &step.observation {
                let referenced_id = observation
                    .extra
                    .get("tool_use_id")
                    .or_else(|| observation.extra.get("id"))
                    .and_then(Value::as_str);
                if let Some(referenced_id) = referenced_id {
                    if !step_call_ids.contains(referenced_id) {
                        return Err(InputIssue::invalid(format!(
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

#[cfg(test)]
pub fn parse_actf_document(input: &str) -> InputResult<ActfDocument> {
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
    fn accepts_name_arguments_tool_without_type_or_id() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        let tool = json!({"name": "Glob", "arguments": {"pattern": "**/*"}});
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"] = json!([tool]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"] =
            json!([tool]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["observation"] =
            json!([{"role":"tool","text":"ok"}]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        document.validate().unwrap();
        let call = &document.attempts["1"].trajectory.steps[0].tools[0];
        assert_eq!(call.kind, "");
        assert_eq!(call.id, "");
        assert_eq!(call.extra["name"], "Glob");
        assert_eq!(call.effective_id(1, 0), "step-1-tool-0");
    }

    #[test]
    fn accepts_object_ground_truth() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["attempts"]["1"]["ground_truth"] = json!({"checklist_path": "/tmp/check.json"});
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        assert_eq!(
            document.attempts["1"].ground_truth,
            json!({"checklist_path": "/tmp/check.json"})
        );
        document.validate().unwrap();
    }

    #[test]
    fn parses_and_validates_actf_v1() {
        let document = fixture();
        document.validate().unwrap();
        let json = document.to_json_string_pretty().unwrap();
        assert_eq!(ActfDocument::from_json_str(&json).unwrap(), document);
    }

    #[test]
    fn accepts_empty_tools_when_assistant_has_tool_calls() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["tools"] = json!([]);
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]["tool_calls"] = json!([{
            "id": "call-1",
            "type": "function",
            "function": {"name": "bash_command", "arguments": {"keystrokes": "pwd\n"}}
        }]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        document.validate().unwrap();
        assert_eq!(
            document.attempts["1"].trajectory.steps[0]
                .effective_tools()
                .len(),
            1
        );
        assert_eq!(
            document.attempts["1"].trajectory.steps[0].effective_tools()[0].id,
            "call-1"
        );
    }

    #[test]
    fn accepts_observation_without_type() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["observation"] = json!([{"content":"ok"}]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        assert_eq!(
            document.attempts["1"].trajectory.steps[0].observation[0].kind,
            ""
        );
        assert_eq!(
            document.attempts["1"].trajectory.steps[0].observation[0].extra["content"],
            "ok"
        );
        document.validate().unwrap();
    }

    #[test]
    fn accepts_openclaw_event_log_as_trajectory() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["attempts"]["1"]["status"] = json!("run_error");
        value["attempts"]["1"]["trajectory"] = json!([
            {"type":"session","id":"s1","timestamp":"2026-06-17T07:26:27.170Z","cwd":"/root"},
            {"type":"message","timestamp":"2026-06-17T07:26:28Z",
             "message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
        ]);
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        document.validate().unwrap();
        assert!(document.attempts["1"].trajectory.steps.is_empty());
        assert_eq!(document.attempts["1"].trajectory.events.len(), 2);
        assert_eq!(
            document.attempts["1"].trajectory.started_at,
            "2026-06-17T07:26:27.170Z"
        );
    }

    #[test]
    fn treats_null_reasoning_content_as_empty_string() {
        let mut value = serde_json::to_value(fixture()).unwrap();
        value["attempts"]["1"]["trajectory"]["steps"][0]["assistant_content"]
            ["reasoning_content"] = Value::Null;
        let document: ActfDocument = serde_json::from_value(value).unwrap();
        assert_eq!(
            document.attempts["1"].trajectory.steps[0]
                .assistant_content
                .reasoning_content,
            ""
        );
        document.validate().unwrap();
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
