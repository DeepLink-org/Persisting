//! `storyline` — ATIF-aligned **hub interchange** format.
//!
//! Root ≈ ATIF Trajectory; `turns[]` ≈ ATIF `steps[]`.
//! Short wire keys (`src`, `msg`, `ts`, …); timing convenience fields
//! (`latency_ms` / `ttft_ms` / `duration_ms`) lift common metrics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::unknown_fields::{compute_unknown_key_counts, StorylineUnknownFields, UnknownKeyCounts};
use crate::{InputIssue, InputResult, Result};

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
    #[serde(default, skip_serializing_if = "StorylineUnknownFields::is_empty")]
    pub unknown_fields: StorylineUnknownFields,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_key_counts: UnknownKeyCounts,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
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
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn validate(&self) -> InputResult<()> {
        if self.session_id.is_empty() {
            return Err(InputIssue::invalid("storyline.session is required"));
        }
        if self.agent.id.is_empty() {
            return Err(InputIssue::invalid("storyline.agent.id is required"));
        }
        if compute_unknown_key_counts(&self.unknown_fields)? != self.unknown_key_counts {
            return Err(InputIssue::invalid(
                "storyline unknown_key_counts do not match unknown_fields",
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

    pub fn refresh_unknown_key_counts(&mut self) -> InputResult<()> {
        self.unknown_key_counts = compute_unknown_key_counts(&self.unknown_fields)?;
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
    fn unsupported_long_field_names_are_rejected() {
        let document = serde_json::json!({
            "session_id": "session-1",
            "agent": { "id": "agent-1" },
            "turns": []
        });
        assert!(serde_json::from_value::<StorylineDocument>(document).is_err());
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
}
