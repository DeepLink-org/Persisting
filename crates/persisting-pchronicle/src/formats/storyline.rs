//! `storyline` — ATIF-aligned **hub interchange** format.
//!
//! Root ≈ ATIF Trajectory; `turns[]` ≈ ATIF `steps[]`.
//! Short wire keys (`src`, `msg`, `ts`, …); timing convenience fields
//! (`latency_ms` / `ttft_ms` / `duration_ms`) lift common metrics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

pub const STORYLINE_SCHEMA_VERSION: &str = "storyline/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineDocument {
    #[serde(rename = "spec", alias = "schema_version")]
    pub schema_version: String,
    #[serde(
        rename = "run",
        alias = "run_id",
        alias = "trajectory_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub run_id: Option<String>,
    /// Session id (≈ ATIF / Capture `session_id`). Wire key: `session`.
    #[serde(rename = "session", alias = "session_id", alias = "story_id")]
    pub session_id: String,
    pub agent: StorylineAgent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<StoryLink>,
    #[serde(
        rename = "children",
        alias = "child_session_ids",
        alias = "child_story_ids",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub child_session_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,
    #[serde(
        alias = "continued_story_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    pub turns: Vec<StorylineTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorylineAgent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        rename = "ver",
        alias = "version",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<String>,
    #[serde(
        rename = "model",
        alias = "model_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_name: Option<String>,
    #[serde(
        rename = "tools",
        alias = "tool_definitions",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_definitions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// Optional parent-session link (ATIF `subagent_trajectories` externalization).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryLink {
    #[serde(
        rename = "psid",
        alias = "parent_session_id",
        alias = "parent_story_id"
    )]
    pub parent_session_id: String,
    #[serde(
        rename = "scid",
        alias = "spawn_call_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub spawn_call_id: Option<String>,
    #[serde(
        rename = "ptid",
        alias = "spawn_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub spawn_id: Option<i64>,
    #[serde(
        rename = "rel",
        alias = "relation",
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
    #[serde(
        rename = "ts",
        alias = "timestamp",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub timestamp: Option<String>,
    #[serde(rename = "src", alias = "source")]
    pub source: String,
    #[serde(rename = "msg", alias = "message")]
    pub message: Value,
    #[serde(
        rename = "reason",
        alias = "reasoning_content",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_content: Option<String>,
    #[serde(
        rename = "effort",
        alias = "reasoning_effort",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<StorylineToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Value>,
    #[serde(
        rename = "model",
        alias = "model_name",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub model_name: Option<String>,
    #[serde(
        rename = "nllm",
        alias = "llm_call_count",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub llm_call_count: Option<i64>,
    #[serde(
        rename = "copied",
        alias = "is_copied_context",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
    #[serde(rename = "tcid", alias = "tool_call_id")]
    pub tool_call_id: String,
    #[serde(rename = "fn", alias = "function_name")]
    pub function_name: String,
    #[serde(rename = "args", alias = "arguments")]
    pub arguments: Value,
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
            schema_version: STORYLINE_SCHEMA_VERSION.into(),
            run_id: None,
            session_id: session_id.into(),
            agent: StorylineAgent {
                id: agent_id.clone(),
                name: Some(agent_id),
                version: Some("0".into()),
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
            turns: Vec::new(),
        }
    }

    pub fn from_json_str(s: &str) -> Result<Self> {
        let doc: Self = serde_json::from_str(s)?;
        doc.validate()?;
        Ok(doc)
    }

    pub fn to_json_string_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.session_id.is_empty() {
            return Err(Error::Other("storyline.session is required".into()));
        }
        if self.agent.id.is_empty() {
            return Err(Error::Other("storyline.agent.id is required".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for turn in &self.turns {
            if turn.source.is_empty() {
                return Err(Error::Other(format!("turn id={} src is required", turn.id)));
            }
            if !seen.insert(turn.id) {
                return Err(Error::Other(format!("duplicate turn id {}", turn.id)));
            }
        }
        Ok(())
    }
}

pub fn parse_storyline_document(input: &str) -> Result<StorylineDocument> {
    StorylineDocument::from_json_str(input)
}
