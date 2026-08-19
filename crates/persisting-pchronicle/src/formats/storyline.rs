//! `storyline` — ATIF-aligned **hub interchange** format.
//!
//! Root ≈ ATIF Trajectory; `turns[]` ≈ ATIF `steps[]`.
//! Short wire keys (`src`, `msg`, `ts`, …); timing convenience fields
//! (`latency_ms` / `ttft_ms` / `duration_ms`) lift common metrics.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{InputIssue, InputResult, Result};

/// Presence semantics for interchange fields where missing and explicit null
/// carry different meanings.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FieldPresence<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> FieldPresence<T> {
    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }

    pub fn as_ref(&self) -> Option<&T> {
        self.value()
    }

    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }
}

impl<T: Serialize> Serialize for FieldPresence<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Missing | Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for FieldPresence<T> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    Missing,
    Null,
    #[default]
    Value,
}

/// Shape of the physical document collection that contained a Storyline.
///
/// This is format-neutral collection semantics, not an editable
/// format-specific residual. It allows collection shape and ordering to pass
/// through the authoritative Storyline model and Lance storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorylineCollectionShape {
    Single,
    Sequence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorylineRootField {
    TrajectoryId,
    Notes,
    FinalMetrics,
    ContinuedTrajectoryRef,
    Extra,
    SubagentTrajectories,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorylineAgentField {
    ModelName,
    ToolDefinitions,
    Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorylineTurnField {
    Timestamp,
    ModelName,
    ReasoningEffort,
    ReasoningContent,
    ToolCalls,
    Observation,
    Metrics,
    Extra,
    LlmCallCount,
    IsCopiedContext,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorylinePresence {
    #[serde(default, skip_serializing_if = "is_value_presence")]
    pub session_id: PresenceState,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub root_nulls: BTreeSet<StorylineRootField>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub agent_nulls: BTreeSet<StorylineAgentField>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub turn_nulls: BTreeMap<i64, BTreeSet<StorylineTurnField>>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tool_call_extra_nulls: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_shape: Option<StorylineCollectionShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_ordinal: Option<i64>,
}

fn is_value_presence(value: &PresenceState) -> bool {
    *value == PresenceState::Value
}

impl StorylinePresence {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(rename = "run", default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(
        rename = "trajectory",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub trajectory_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// Session id (≈ ATIF / Capture `session_id`). Wire key: `session`.
    #[serde(rename = "session")]
    pub session_id: String,
    pub agent: StorylineAgent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<StoryLink>,
    #[serde(rename = "children", default, skip_serializing_if = "Option::is_none")]
    pub child_session_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "StorylinePresence::is_default")]
    pub presence: StorylinePresence,
    pub turns: Vec<StorylineTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineAgent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "ver", default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(rename = "model", default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(rename = "tools", default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// Optional parent-session link (ATIF `subagent_trajectories` externalization).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryLink {
    #[serde(rename = "psid")]
    pub parent_session_id: String,
    #[serde(rename = "scid", default, skip_serializing_if = "Option::is_none")]
    pub spawn_call_id: Option<String>,
    #[serde(rename = "ptid", default, skip_serializing_if = "Option::is_none")]
    pub spawn_id: Option<i64>,
    #[serde(
        rename = "rel",
        default = "default_spawn",
        skip_serializing_if = "is_default_spawn"
    )]
    pub relation: String,
}

fn is_default_spawn(s: &str) -> bool {
    s == "spawn"
}

fn default_spawn() -> String {
    "spawn".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineTurn {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(rename = "ts", default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(rename = "src")]
    pub source: String,
    #[serde(rename = "msg")]
    pub message: Value,
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(rename = "effort", default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<StorylineToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(rename = "model", default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(rename = "nllm", default, skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<i64>,
    #[serde(rename = "copied", default, skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
    /// End-to-end LLM round-trip latency in milliseconds (peer response time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    /// Time to first token in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl StorylineTurn {
    pub fn effective_kind(&self) -> &str {
        if let Some(k) = self.kind.as_deref() {
            return k;
        }
        match self.source.as_str() {
            "user" => "dialogue",
            "system" => "internal",
            "agent"
                if self
                    .tool_calls
                    .as_ref()
                    .map(|c| !c.is_empty())
                    .unwrap_or(false) =>
            {
                "autonomous"
            }
            _ => "dialogue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineToolCall {
    #[serde(rename = "tcid")]
    pub tool_call_id: String,
    #[serde(rename = "fn")]
    pub function_name: String,
    #[serde(rename = "args")]
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "FieldPresence::is_missing")]
    pub result: FieldPresence<Value>,
    /// Tool execution wall time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

impl StorylineDocument {
    pub fn new(session_id: impl Into<String>, agent_id: impl Into<String>) -> Self {
        let agent_id = agent_id.into();
        Self {
            schema_version: None,
            run_id: None,
            trajectory_id: None,
            attempt_id: None,
            session_id: session_id.into(),
            agent: StorylineAgent {
                id: agent_id.clone(),
                name: Some(agent_id),
                version: None,
                model_name: None,
                tool_definitions: None,
                extra: None,
            },
            parent: None,
            child_session_ids: None,
            notes: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            presence: StorylinePresence::default(),
            turns: Vec::new(),
        }
    }

    /// Stable identity used by Storyline storage and normalized table joins.
    pub fn document_id(&self) -> &str {
        self.trajectory_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&self.session_id)
    }

    pub fn from_json_str(s: &str) -> InputResult<Self> {
        let doc: Self =
            serde_json::from_str(s).map_err(|error| InputIssue::invalid(error.to_string()))?;
        doc.validate()?;
        Ok(doc)
    }

    pub fn to_json_string_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> InputResult<()> {
        if self.session_id.is_empty() {
            return Err(InputIssue::invalid("storyline.session is required"));
        }
        if self.agent.id.is_empty() {
            return Err(InputIssue::invalid("storyline.agent.id is required"));
        }
        if self
            .presence
            .collection_ordinal
            .is_some_and(|ordinal| ordinal < 0)
        {
            return Err(InputIssue::invalid(
                "storyline collection ordinal cannot be negative",
            ));
        }
        if self.presence.collection_shape == Some(StorylineCollectionShape::Single)
            && self
                .presence
                .collection_ordinal
                .is_some_and(|ordinal| ordinal != 0)
        {
            return Err(InputIssue::invalid(
                "single-document Storyline collection ordinal must be zero",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for turn in &self.turns {
            if turn.source.is_empty() {
                return Err(InputIssue::invalid(format!(
                    "turn id={} src is required",
                    turn.id
                )));
            }
            if !seen.insert(turn.id) {
                return Err(InputIssue::invalid(format!(
                    "duplicate turn id {}",
                    turn.id
                )));
            }
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "lance-store"))]
pub fn parse_storyline_document(input: &str) -> Result<StorylineDocument> {
    StorylineDocument::from_json_str(input).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_long_field_names_are_rejected() {
        let legacy = serde_json::json!({
            "session_id": "session-1",
            "agent": { "id": "agent-1" },
            "turns": []
        });
        assert!(serde_json::from_value::<StorylineDocument>(legacy).is_err());
    }

    #[test]
    fn tool_result_presence_distinguishes_missing_null_and_value() {
        let base = serde_json::json!({"tcid":"call-1","fn":"lookup","args":{}});

        let missing: StorylineToolCall = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(missing.result, FieldPresence::Missing);
        assert!(serde_json::to_value(missing)
            .unwrap()
            .get("result")
            .is_none());

        let mut null = base.clone();
        null["result"] = Value::Null;
        let null: StorylineToolCall = serde_json::from_value(null).unwrap();
        assert_eq!(null.result, FieldPresence::Null);
        assert_eq!(serde_json::to_value(null).unwrap()["result"], Value::Null);

        let mut value = base;
        value["result"] = serde_json::json!({"answer": 42});
        let value: StorylineToolCall = serde_json::from_value(value).unwrap();
        assert_eq!(
            value.result,
            FieldPresence::Value(serde_json::json!({"answer": 42}))
        );
        assert_eq!(
            serde_json::to_value(value).unwrap()["result"],
            serde_json::json!({"answer": 42})
        );
    }
}
