//! `storyline` — ATIF-aligned **hub interchange** format.
//!
//! Root ≈ ATIF Trajectory; `turns[]` ≈ ATIF `steps[]`.
//! Short wire keys (`src`, `msg`, `ts`, …); timing convenience fields
//! (`latency_ms` / `ttft_ms` / `duration_ms`) lift common metrics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::timestamp::StorylineTimestamp;
use super::unknown_fields::{compute_unknown_key_counts, StorylineUnknownFields, UnknownKeyCounts};
use crate::{InputIssue, InputResult, Result};

pub const STORYLINE_SCHEMA_VERSION: &str = "storyline/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineDocument {
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<StorylineOrigin>,
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
    pub task: Option<StorylineTask>,
    #[serde(default, skip_serializing_if = "skip_optional_empty_prompt")]
    pub prompt: Option<StorylinePrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<StorylineTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<StorylineTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(default, skip_serializing_if = "StorylineUnknownFields::is_empty")]
    pub unknown_fields: StorylineUnknownFields,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_key_counts: UnknownKeyCounts,
    pub turns: Vec<StorylineTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineOrigin {
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct StorylineTurn {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(rename = "ts", default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<StorylineTimestamp>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<StorylineEnv>,
    #[serde(default, skip_serializing_if = "skip_turn_prompt")]
    pub prompt: Option<StorylinePrompt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<StorylineTimestamp>,
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
#[serde(deny_unknown_fields)]
pub struct StorylineToolCall {
    #[serde(rename = "tcid")]
    pub tool_call_id: String,
    #[serde(rename = "fn")]
    pub function_name: String,
    #[serde(rename = "args")]
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Tool execution wall time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<StorylineToolResponse>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineEnv {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_empty_map")]
    pub state: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylinePrompt {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
}

impl StorylinePrompt {
    pub fn from_pair(system: &str, user: &str) -> Option<Self> {
        if system.is_empty() && user.is_empty() {
            return None;
        }
        Some(Self {
            system: (!system.is_empty()).then(|| system.to_string()),
            user: (!user.is_empty()).then(|| user.to_string()),
        })
    }

    pub fn explicit_clear() -> Self {
        Self {
            system: Some(String::new()),
            user: Some(String::new()),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.system.as_deref().is_none_or(str::is_empty)
            && self.user.as_deref().is_none_or(str::is_empty)
    }

    pub fn is_explicit_clear(&self) -> bool {
        self.system.as_deref() == Some("") && self.user.as_deref() == Some("")
    }

    pub fn has_nonempty_field(&self) -> bool {
        self.system
            .as_deref()
            .is_some_and(|value| !value.is_empty())
            || self.user.as_deref().is_some_and(|value| !value.is_empty())
    }

    pub fn pair(&self) -> (String, String) {
        (
            self.system.clone().unwrap_or_default(),
            self.user.clone().unwrap_or_default(),
        )
    }
}

impl Serialize for StorylinePrompt {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if self.is_explicit_clear() {
            let mut state = serializer.serialize_struct("StorylinePrompt", 2)?;
            state.serialize_field("system", "")?;
            state.serialize_field("user", "")?;
            return state.end();
        }
        let system = self.system.as_deref().filter(|value| !value.is_empty());
        let user = self.user.as_deref().filter(|value| !value.is_empty());
        let mut state = serializer.serialize_struct(
            "StorylinePrompt",
            usize::from(system.is_some()) + usize::from(user.is_some()),
        )?;
        if let Some(system) = system {
            state.serialize_field("system", system)?;
        }
        if let Some(user) = user {
            state.serialize_field("user", user)?;
        }
        state.end()
    }
}

fn skip_optional_empty_prompt(prompt: &Option<StorylinePrompt>) -> bool {
    prompt.as_ref().is_none_or(StorylinePrompt::is_empty)
}

fn skip_turn_prompt(prompt: &Option<StorylinePrompt>) -> bool {
    match prompt {
        None => true,
        Some(prompt) if prompt.is_explicit_clear() => false,
        Some(prompt) => prompt.is_empty(),
    }
}

impl StorylineEnv {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.endpoint.is_none()
            && self.id.is_none()
            && self.event_type.is_none()
            && self.request_id.is_none()
            && self.state.as_ref().is_none_or(serde_json::Map::is_empty)
    }

    pub fn merge_overlay(&self, overlay: &Self) -> Self {
        let mut state = self.state.clone().unwrap_or_default();
        if let Some(overlay_state) = &overlay.state {
            for (key, value) in overlay_state {
                state.insert(key.clone(), value.clone());
            }
        }
        Self {
            name: overlay.name.clone().or_else(|| self.name.clone()),
            endpoint: overlay.endpoint.clone().or_else(|| self.endpoint.clone()),
            id: overlay.id.clone().or_else(|| self.id.clone()),
            event_type: overlay
                .event_type
                .clone()
                .or_else(|| self.event_type.clone()),
            request_id: overlay
                .request_id
                .clone()
                .or_else(|| self.request_id.clone()),
            state: (!state.is_empty()).then_some(state),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineTaskLlm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<i64>,
}

impl StorylineTaskLlm {
    pub fn is_empty(&self) -> bool {
        self.k.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineTaskResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_correct: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correct: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_answer: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground_truth: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts_tried: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_counts: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_score: Option<Value>,
}

impl StorylineTaskResult {
    pub fn is_empty(&self) -> bool {
        self.task_correct.is_none()
            && self.correct.is_none()
            && self.final_answer.is_none()
            && self.ground_truth.is_none()
            && self.status.is_none()
            && self.score.is_none()
            && self.error.is_none()
            && self.artifacts.is_none()
            && self.category.is_none()
            && self.attempts_tried.is_none()
            && self.solved_at.is_none()
            && self.retry_count.is_none()
            && self.retry_counts.is_none()
            && self.max_score.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<StorylineEnv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<StorylineTaskLlm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<StorylineTaskResult>,
}

impl StorylineTask {
    pub fn is_empty(&self) -> bool {
        self.env.as_ref().is_none_or(StorylineEnv::is_empty)
            && self.llm.as_ref().is_none_or(StorylineTaskLlm::is_empty)
            && self
                .result
                .as_ref()
                .is_none_or(StorylineTaskResult::is_empty)
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorylineToolResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
}

impl StorylineToolResponse {
    pub fn is_empty(&self) -> bool {
        self.status.is_none() && self.exit_code.is_none()
    }
}

fn skip_empty_map(map: &Option<serde_json::Map<String, Value>>) -> bool {
    map.as_ref().is_none_or(serde_json::Map::is_empty)
}

impl StorylineDocument {
    pub fn new(session_id: impl Into<String>, agent_id: impl Into<String>) -> Self {
        let agent_id = agent_id.into();
        Self {
            schema_version: STORYLINE_SCHEMA_VERSION.into(),
            origin: None,
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
            task: None,
            prompt: None,
            started_at: None,
            finished_at: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            extra: None,
            meta: None,
            unknown_fields: StorylineUnknownFields::default(),
            unknown_key_counts: UnknownKeyCounts::default(),
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
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> InputResult<()> {
        if self.schema_version != STORYLINE_SCHEMA_VERSION {
            return Err(InputIssue::unsupported(format!(
                "unsupported storyline schema_version '{}'; expected {}",
                self.schema_version, STORYLINE_SCHEMA_VERSION
            )));
        }
        if self.session_id.is_empty() {
            return Err(InputIssue::invalid("storyline.session is required"));
        }
        if self
            .trajectory_id
            .as_ref()
            .is_some_and(|trajectory_id| trajectory_id.is_empty())
        {
            return Err(InputIssue::invalid(
                "storyline.trajectory must be non-empty when present",
            ));
        }
        if self.agent.id.is_empty() {
            return Err(InputIssue::invalid("storyline.agent.id is required"));
        }
        if let Some(task) = &self.task {
            if task.is_empty() {
                return Err(InputIssue::invalid(
                    "storyline.task must contain env, llm, or result",
                ));
            }
            if let Some(k) = task.llm.as_ref().and_then(|llm| llm.k) {
                if k <= 0 {
                    return Err(InputIssue::invalid("storyline.task.llm.k must be positive"));
                }
            }
        }
        if let Some(prompt) = &self.prompt {
            if !prompt.has_nonempty_field() {
                return Err(InputIssue::invalid(
                    "storyline.prompt must contain a non-empty system or user",
                ));
            }
        }
        if let Some(origin) = &self.origin {
            if origin.format.is_empty() {
                return Err(InputIssue::invalid(
                    "storyline.origin.format must be non-empty",
                ));
            }
            if origin
                .schema_version
                .as_ref()
                .is_some_and(|version| version.is_empty())
            {
                return Err(InputIssue::invalid(
                    "storyline.origin.schema_version must be non-empty when present",
                ));
            }
            if origin
                .document_id
                .as_ref()
                .is_some_and(|document_id| document_id.is_empty())
            {
                return Err(InputIssue::invalid(
                    "storyline.origin.document_id must be non-empty when present",
                ));
            }
        }
        if compute_unknown_key_counts(&self.unknown_fields)? != self.unknown_key_counts {
            return Err(InputIssue::invalid(
                "storyline unknown_key_counts do not match unknown_fields",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        let mut seen_tool_calls = std::collections::HashSet::new();
        for turn in &self.turns {
            if turn.source.is_empty() {
                return Err(InputIssue::invalid(format!(
                    "turn id={} src is required",
                    turn.id
                )));
            }
            if let Some(prompt) = &turn.prompt {
                if turn.is_copied_context == Some(true) {
                    return Err(InputIssue::invalid(format!(
                        "turn id={} copied context must not contain prompt",
                        turn.id
                    )));
                }
                if prompt.is_explicit_clear() {
                    // Whole-document replace to empty strings.
                } else if !prompt.has_nonempty_field() {
                    return Err(InputIssue::invalid(format!(
                        "turn id={} prompt must contain a non-empty system or user, or explicit empty system and user",
                        turn.id
                    )));
                }
            }
            if !seen.insert(turn.id) {
                return Err(InputIssue::invalid(format!(
                    "duplicate turn id {}",
                    turn.id
                )));
            }
            for call in turn.tool_calls.as_deref().unwrap_or_default() {
                if call.tool_call_id.is_empty() {
                    return Err(InputIssue::invalid(format!(
                        "turn id={} tool_call_id must be non-empty",
                        turn.id
                    )));
                }
                if call.function_name.is_empty() {
                    return Err(InputIssue::invalid(format!(
                        "turn id={} function_name must be non-empty",
                        turn.id
                    )));
                }
                if call
                    .response
                    .as_ref()
                    .is_some_and(StorylineToolResponse::is_empty)
                {
                    return Err(InputIssue::invalid(format!(
                        "turn id={} tool_call response must contain status or exit_code",
                        turn.id
                    )));
                }
                if !seen_tool_calls.insert(call.tool_call_id.as_str()) {
                    return Err(InputIssue::invalid(format!(
                        "duplicate tool_call_id {}",
                        call.tool_call_id
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn refresh_unknown_key_counts(&mut self) -> InputResult<()> {
        self.unknown_key_counts = compute_unknown_key_counts(&self.unknown_fields)?;
        Ok(())
    }

    pub fn effective_prompt<'a>(&'a self, turn: &'a StorylineTurn) -> Option<&'a StorylinePrompt> {
        turn.prompt.as_ref().or(self.prompt.as_ref())
    }
}

#[cfg(all(test, feature = "lance-store"))]
pub fn parse_storyline_document(input: &str) -> Result<StorylineDocument> {
    StorylineDocument::from_json_str(input).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn story_with_source_normalized_counts() -> StorylineDocument {
        let mut story = StorylineDocument::new("session", "agent");
        for (source, source_id, pointer) in [
            ("atif", "atif-doc", "/steps/0/vendor"),
            ("atif", "atif-doc", "/steps/1/vendor"),
            ("actf", "actf-doc", "/attempts/7/trajectory/steps/0/vendor"),
            ("actf", "actf-doc", "/attempts/7/trajectory/steps/1/vendor"),
            (
                "openai-msg",
                "openai-doc",
                "/session_steps/0/messages/0/vendor",
            ),
            (
                "openai-msg",
                "openai-doc",
                "/session_steps/1/messages/1/vendor",
            ),
            ("agenticmd", "agenticmd-doc", "/blocks/0/header/vendor"),
            ("agenticmd", "agenticmd-doc", "/blocks/1/header/vendor"),
            ("future-format", "future-doc", "/items/0/vendor"),
            ("future-format", "future-doc", "/items/1/vendor"),
        ] {
            story
                .unknown_fields
                .insert(source, source_id, pointer, serde_json::json!(true))
                .unwrap();
        }
        story.unknown_key_counts = BTreeMap::from([
            (
                "atif".into(),
                BTreeMap::from([("/steps/*/vendor".into(), 2)]),
            ),
            (
                "actf".into(),
                BTreeMap::from([("/attempts/7/trajectory/steps/*/vendor".into(), 2)]),
            ),
            (
                "openai-msg".into(),
                BTreeMap::from([("/session_steps/*/messages/*/vendor".into(), 2)]),
            ),
            (
                "agenticmd".into(),
                BTreeMap::from([("/blocks/*/header/vendor".into(), 2)]),
            ),
            (
                "future-format".into(),
                BTreeMap::from([("/items/0/vendor".into(), 1), ("/items/1/vendor".into(), 1)]),
            ),
        ]);
        story
    }

    #[test]
    fn validate_accepts_source_normalized_unknown_key_counts() {
        story_with_source_normalized_counts().validate().unwrap();
    }

    #[test]
    fn validate_rejects_stale_source_normalized_unknown_key_counts() {
        let mut story = story_with_source_normalized_counts();
        *story
            .unknown_key_counts
            .get_mut("atif")
            .unwrap()
            .get_mut("/steps/*/vendor")
            .unwrap() = 1;
        assert!(story.validate().is_err());
    }

    #[test]
    fn refresh_counts_wildcards_only_schema_array_positions() {
        let mut story = StorylineDocument::new("session", "agent");
        for (source, source_id, pointer) in [
            ("atif", "atif-doc", "/steps/0/0"),
            ("actf", "actf-doc", "/attempts/1/trajectory/steps/0/0"),
            ("openai-msg", "openai-doc", "/session_steps/0/messages/0/0"),
            ("agenticmd", "agenticmd-doc", "/frontmatter/0"),
            ("agenticmd", "agenticmd-doc", "/blocks/0/header/0"),
            ("future-format", "future-doc", "/items/0/0"),
        ] {
            story
                .unknown_fields
                .insert(source, source_id, pointer, serde_json::json!(true))
                .unwrap();
        }

        story.refresh_unknown_key_counts().unwrap();

        assert_eq!(story.unknown_key_counts["atif"]["/steps/*/0"], 1);
        assert_eq!(
            story.unknown_key_counts["actf"]["/attempts/1/trajectory/steps/*/0"],
            1
        );
        assert_eq!(
            story.unknown_key_counts["openai-msg"]["/session_steps/*/messages/*/0"],
            1
        );
        assert_eq!(story.unknown_key_counts["agenticmd"]["/frontmatter/0"], 1);
        assert_eq!(
            story.unknown_key_counts["agenticmd"]["/blocks/*/header/0"],
            1
        );
        assert_eq!(story.unknown_key_counts["future-format"]["/items/0/0"], 1);
        story.validate().unwrap();
    }

    #[test]
    fn storyline_serialization_omits_empty_unknown_fields() {
        let story = StorylineDocument::new("session", "agent");
        let value = serde_json::to_value(story).unwrap();
        assert!(value.get("unknown_fields").is_none());
    }

    #[test]
    fn storyline_wire_requires_the_supported_version() {
        let missing = serde_json::json!({
            "session": "session-1",
            "agent": { "id": "agent-1" },
            "turns": []
        });
        let unsupported = serde_json::json!({
            "schema_version": "storyline/v2",
            "session": "session-1",
            "agent": { "id": "agent-1" },
            "turns": []
        });

        assert!(StorylineDocument::from_json_str(&missing.to_string()).is_err());
        assert!(StorylineDocument::from_json_str(&unsupported.to_string()).is_err());
    }

    #[test]
    fn storyline_wire_rejects_unknown_owned_fields() {
        let unknown_root = serde_json::json!({
            "schema_version": "storyline/v1",
            "session": "session-1",
            "agent": { "id": "agent-1" },
            "turns": [],
            "session_id": "long-key-must-not-be-ignored"
        });
        let unknown_turn = serde_json::json!({
            "schema_version": "storyline/v1",
            "session": "session-1",
            "agent": { "id": "agent-1" },
            "turns": [{
                "id": 1,
                "src": "user",
                "msg": "hello",
                "source": "long-key-must-not-be-ignored"
            }]
        });

        assert!(StorylineDocument::from_json_str(&unknown_root.to_string()).is_err());
        assert!(StorylineDocument::from_json_str(&unknown_turn.to_string()).is_err());
    }

    #[test]
    fn typed_timestamp_preserves_fractional_epoch_source_and_instant() {
        let timestamp =
            crate::model::StorylineTimestamp::from_json(serde_json::json!(1785578400.25)).unwrap();

        assert_eq!(timestamp.timestamp_nanos(), 1_785_578_400_250_000_000);
        assert_eq!(timestamp.source_value(), &serde_json::json!(1785578400.25));
        assert_eq!(
            serde_json::to_value(&timestamp).unwrap(),
            serde_json::json!(1785578400.25)
        );
    }

    #[test]
    fn typed_timestamp_parses_integer_negative_and_exponent_epoch_values_exactly() {
        for (source, expected_nanos) in [
            (serde_json::json!(0), 0),
            (serde_json::json!(-1.25), -1_250_000_000),
            (serde_json::from_str("1e-9").unwrap(), 1),
        ] {
            let timestamp = crate::model::StorylineTimestamp::from_json(source.clone()).unwrap();
            assert_eq!(timestamp.timestamp_nanos(), expected_nanos);
            assert_eq!(timestamp.source_value(), &source);
        }
    }

    #[test]
    fn typed_timestamp_normalizes_instant_without_rewriting_source_text() {
        let offset = crate::model::StorylineTimestamp::from_json(serde_json::json!(
            "2026-08-20T08:00:00.123456789+08:00"
        ))
        .unwrap();
        let utc = crate::model::StorylineTimestamp::from_json(serde_json::json!(
            "2026-08-20T00:00:00.123456789Z"
        ))
        .unwrap();

        assert_eq!(offset.instant(), utc.instant());
        assert_eq!(
            offset.source_value(),
            &serde_json::json!("2026-08-20T08:00:00.123456789+08:00")
        );
        assert_eq!(offset.canonical_rfc3339(), "2026-08-20T00:00:00.123456789Z");
    }

    #[test]
    fn typed_timestamp_rejects_sub_nanosecond_epoch_values() {
        let error = crate::model::StorylineTimestamp::from_json(serde_json::json!(1.0000000001))
            .unwrap_err();

        assert!(error.to_string().contains("nanosecond"), "{error}");
    }

    #[test]
    fn typed_timestamp_rejects_epoch_values_outside_nanosecond_range() {
        let error =
            crate::model::StorylineTimestamp::from_json(serde_json::json!(10_000_000_000u64))
                .unwrap_err();

        assert!(error.to_string().contains("range"), "{error}");
    }

    #[test]
    fn typed_timestamp_rejects_non_timestamp_json_scalars() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(true),
            serde_json::json!("2026/08/20 00:00:00"),
        ] {
            assert!(crate::model::StorylineTimestamp::from_json(value).is_err());
        }
    }

    #[test]
    fn storyline_decode_rejects_non_rfc3339_timestamps() {
        let input = serde_json::json!({
            "schema_version": STORYLINE_SCHEMA_VERSION,
            "session": "session",
            "agent": {"id": "agent"},
            "turns": [{
                "id": 1,
                "ts": "2026/08/20 12:00:00",
                "src": "user",
                "msg": "hello"
            }]
        });

        let error = StorylineDocument::from_json_str(&input.to_string()).unwrap_err();
        assert!(error.to_string().contains("RFC3339"), "{error}");
    }

    #[test]
    fn storyline_validation_rejects_duplicate_tool_call_ids() {
        let mut story = StorylineDocument::new("session", "agent");
        let call = StorylineToolCall {
            tool_call_id: "call-1".into(),
            function_name: "lookup".into(),
            arguments: serde_json::json!({}),
            result: None,
            duration_ms: None,
            extra: None,
            kind: None,
            response: None,
        };
        for id in [1, 2] {
            story.turns.push(StorylineTurn {
                id,
                kind: None,
                timestamp: None,
                source: "agent".into(),
                message: Value::Null,
                reasoning_content: None,
                reasoning_effort: None,
                tool_calls: Some(vec![call.clone()]),
                observation: None,
                metrics: None,
                model_name: None,
                llm_call_count: None,
                is_copied_context: None,
                latency_ms: None,
                ttft_ms: None,
                extra: None,
                env: None,
                prompt: None,
                finished_at: None,
            });
        }

        let error = story.validate().unwrap_err();
        assert!(
            error.to_string().contains("duplicate tool_call_id"),
            "{error}"
        );
    }

    #[test]
    fn tool_result_canonicalizes_missing_and_null() {
        let base = serde_json::json!({"tcid":"call-1","fn":"lookup","args":{}});

        let missing: StorylineToolCall = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(missing.result, None);
        assert!(serde_json::to_value(missing)
            .unwrap()
            .get("result")
            .is_none());

        let mut null = base.clone();
        null["result"] = Value::Null;
        let null: StorylineToolCall = serde_json::from_value(null).unwrap();
        assert_eq!(null.result, None);
        assert!(serde_json::to_value(null).unwrap().get("result").is_none());

        let mut value = base;
        value["result"] = serde_json::json!({"answer": 42});
        let value: StorylineToolCall = serde_json::from_value(value).unwrap();
        assert_eq!(value.result, Some(serde_json::json!({"answer": 42})));
        assert_eq!(
            serde_json::to_value(value).unwrap()["result"],
            serde_json::json!({"answer": 42})
        );
    }

    #[test]
    fn task_env_and_tool_response_roundtrip_on_the_wire() {
        let mut story = StorylineDocument::new("session", "agent");
        story.started_at = Some(StorylineTimestamp::from_rfc3339("2026-01-01T00:00:00Z").unwrap());
        story.finished_at = Some(StorylineTimestamp::from_rfc3339("2026-01-01T00:00:02Z").unwrap());
        story.task = Some(StorylineTask {
            env: Some(StorylineEnv {
                name: Some("prod".into()),
                endpoint: Some("https://llm".into()),
                state: Some(serde_json::Map::from_iter([(
                    "weight_version".into(),
                    json!("v1"),
                )])),
                ..StorylineEnv::default()
            }),
            llm: Some(StorylineTaskLlm { k: Some(3) }),
            result: Some(StorylineTaskResult {
                correct: Some(true),
                category: Some("software-engineering".into()),
                ..StorylineTaskResult::default()
            }),
        });
        story.turns.push(StorylineTurn {
            id: 1,
            kind: None,
            timestamp: story.started_at.clone(),
            source: "agent".into(),
            message: json!("done"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: Some(vec![StorylineToolCall {
                tool_call_id: "c1".into(),
                function_name: "Bash".into(),
                arguments: json!({}),
                result: Some(json!("ok")),
                duration_ms: None,
                extra: None,
                kind: Some("tool_use".into()),
                response: Some(StorylineToolResponse {
                    status: Some("completed".into()),
                    exit_code: Some(0),
                }),
            }]),
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
            env: Some(StorylineEnv {
                request_id: Some("req-1".into()),
                ..StorylineEnv::default()
            }),
            prompt: None,
            finished_at: story.finished_at.clone(),
        });

        let encoded = serde_json::to_value(&story).unwrap();
        assert_eq!(encoded["task"]["llm"]["k"], 3);
        assert_eq!(encoded["task"]["env"]["name"], "prod");
        assert_eq!(encoded["turns"][0]["env"]["request_id"], "req-1");
        assert_eq!(encoded["turns"][0]["tool_calls"][0]["kind"], "tool_use");
        assert_eq!(
            encoded["turns"][0]["tool_calls"][0]["response"]["exit_code"],
            0
        );
        let decoded: StorylineDocument = serde_json::from_value(encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(
            decoded.task.as_ref().unwrap().llm.as_ref().unwrap().k,
            Some(3)
        );
    }

    #[test]
    fn empty_task_is_rejected() {
        let mut story = StorylineDocument::new("session", "agent");
        story.task = Some(StorylineTask::default());
        let error = story.validate().unwrap_err();
        assert!(error.to_string().contains("task"), "{error}");
    }

    fn agent_turn(id: i64) -> StorylineTurn {
        StorylineTurn {
            id,
            kind: None,
            timestamp: None,
            source: "agent".into(),
            message: json!("done"),
            reasoning_content: None,
            reasoning_effort: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            model_name: None,
            llm_call_count: None,
            is_copied_context: None,
            latency_ms: None,
            ttft_ms: None,
            extra: None,
            env: None,
            prompt: None,
            finished_at: None,
        }
    }

    #[test]
    fn prompt_wire_roundtrip_and_explicit_clear() {
        let mut story = StorylineDocument::new("session", "agent");
        story.prompt = StorylinePrompt::from_pair("sys", "task");
        let mut changed = agent_turn(1);
        changed.prompt = StorylinePrompt::from_pair("sys", "later");
        let mut cleared = agent_turn(2);
        cleared.prompt = Some(StorylinePrompt::explicit_clear());
        story.turns.push(changed);
        story.turns.push(cleared);
        story.validate().unwrap();

        let encoded = serde_json::to_value(&story).unwrap();
        assert_eq!(encoded["prompt"]["system"], "sys");
        assert_eq!(encoded["prompt"]["user"], "task");
        assert_eq!(encoded["turns"][0]["prompt"]["user"], "later");
        assert_eq!(encoded["turns"][0]["prompt"]["system"], "sys");
        assert_eq!(encoded["turns"][1]["prompt"]["system"], "");
        assert_eq!(encoded["turns"][1]["prompt"]["user"], "");
        assert!(encoded["turns"][0]["msg"].is_string());

        let decoded: StorylineDocument = serde_json::from_value(encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(
            decoded.effective_prompt(&decoded.turns[0]).unwrap().pair(),
            ("sys".into(), "later".into())
        );
        assert_eq!(
            decoded.effective_prompt(&decoded.turns[1]).unwrap().pair(),
            (String::new(), String::new())
        );
    }

    #[test]
    fn prompt_validation_rejects_empty_and_copied() {
        let mut story = StorylineDocument::new("session", "agent");
        story.prompt = Some(StorylinePrompt::default());
        let error = story.validate().unwrap_err();
        assert!(error.to_string().contains("prompt"), "{error}");

        let mut story = StorylineDocument::new("session", "agent");
        let mut turn = agent_turn(1);
        turn.prompt = Some(StorylinePrompt::default());
        story.turns.push(turn);
        let error = story.validate().unwrap_err();
        assert!(error.to_string().contains("prompt"), "{error}");

        let mut story = StorylineDocument::new("session", "agent");
        story.prompt = StorylinePrompt::from_pair("sys", "task");
        let mut turn = agent_turn(1);
        turn.is_copied_context = Some(true);
        turn.prompt = StorylinePrompt::from_pair("sys", "task");
        story.turns.push(turn);
        let error = story.validate().unwrap_err();
        assert!(error.to_string().contains("copied"), "{error}");
    }
}
