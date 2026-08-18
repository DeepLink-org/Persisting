//! Relational rows for the Storyline-native, three-table Lance projection.
//!
//! A Storyline document is normalized into one run row, ordered step rows, and
//! tool-call rows. ATIF-compatible `observation.results[]` values are attached
//! to their call through `source_call_id` and stored in `results`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    Error, FieldPresence, Result, StoryLink, StorylineDocument, StorylinePresence,
    StorylineToolCall, StorylineTurn,
};

#[cfg(feature = "lance-store")]
pub const STORY_RUNS_TABLE: &str = "runs";
#[cfg(feature = "lance-store")]
pub const STORY_STEPS_TABLE: &str = "steps";
#[cfg(feature = "lance-store")]
pub const STORY_TOOL_CALLS_TABLE: &str = "tool_calls";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryRunRow {
    pub schema_version: Option<String>,
    /// Stable per-document storage identity. Explicit ATIF `trajectory_id`
    /// wins; otherwise the effective `session_id` is used.
    pub document_id: String,
    /// Stable global order inside this Storyline store. This is deliberately
    /// distinct from source-container ordinals retained in `presence`.
    pub storage_ordinal: i64,
    pub trajectory_id_explicit: bool,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    pub agent_model_name: Option<String>,
    pub agent_tool_definitions: Option<Value>,
    pub agent_extra: Option<Value>,
    pub parent: Option<StoryLink>,
    pub child_session_ids: Option<Vec<String>>,
    pub notes: Option<String>,
    pub final_metrics: Option<Value>,
    pub continued_trajectory_ref: Option<String>,
    pub extra: Option<Value>,
    pub presence: StorylinePresence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryStepRow {
    pub document_id: String,
    pub run_id: Option<String>,
    pub session_id: String,
    pub step_id: i64,
    pub kind: Option<String>,
    pub effective_kind: String,
    pub timestamp: Option<String>,
    pub source: String,
    pub message: Value,
    pub reasoning_content: Option<String>,
    pub reasoning_effort: Option<Value>,
    pub metrics: Option<Value>,
    pub model_name: Option<String>,
    pub llm_call_count: Option<i64>,
    pub is_copied_context: Option<bool>,
    pub latency_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    /// Keeps `observation: {"results": []}` distinct from no observation.
    pub had_observation: bool,
    pub extra: Option<Value>,
}

/// One row per tool call. `results` keeps zero or more ATIF observation result
/// objects correlated by `source_call_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryToolCallRow {
    pub document_id: String,
    pub run_id: Option<String>,
    pub session_id: String,
    pub step_id: i64,
    pub call_index: i64,
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Value,
    pub result: FieldPresence<Value>,
    pub results: Vec<Value>,
    pub duration_ms: Option<i64>,
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StorylineTables {
    pub run: StoryRunRow,
    pub steps: Vec<StoryStepRow>,
    pub tool_calls: Vec<StoryToolCallRow>,
}

fn observation_results(observation: &Option<Value>) -> Result<&[Value]> {
    let Some(observation) = observation else {
        return Ok(&[]);
    };
    observation
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| Error::Other("storyline observation must contain a results array".into()))
}

fn source_call_id(result: &Value) -> Option<&str> {
    result
        .get("source_call_id")
        .or_else(|| result.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

pub fn split_storyline(story: &StorylineDocument) -> Result<StorylineTables> {
    story.validate()?;
    let document_id = story
        .trajectory_id
        .clone()
        .unwrap_or_else(|| story.session_id.clone());
    let run = StoryRunRow {
        schema_version: story.schema_version.clone(),
        document_id: document_id.clone(),
        storage_ordinal: 0,
        trajectory_id_explicit: story.trajectory_id.is_some(),
        run_id: story.run_id.clone(),
        attempt_id: story.attempt_id.clone(),
        session_id: story.session_id.clone(),
        agent_id: story.agent.id.clone(),
        agent_name: story.agent.name.clone(),
        agent_version: story.agent.version.clone(),
        agent_model_name: story.agent.model_name.clone(),
        agent_tool_definitions: story.agent.tool_definitions.clone(),
        agent_extra: story.agent.extra.clone(),
        parent: story.parent.clone(),
        child_session_ids: story.child_session_ids.clone(),
        notes: story.notes.clone(),
        final_metrics: story.final_metrics.clone(),
        continued_trajectory_ref: story.continued_trajectory_ref.clone(),
        extra: story.extra.clone(),
        presence: story.presence.clone(),
    };

    let mut seen_calls = HashSet::new();
    let mut steps = Vec::with_capacity(story.turns.len());
    let mut tool_calls = Vec::new();
    for turn in &story.turns {
        steps.push(StoryStepRow {
            document_id: document_id.clone(),
            run_id: story.run_id.clone(),
            session_id: story.session_id.clone(),
            step_id: turn.id,
            kind: turn.kind.clone(),
            effective_kind: turn.effective_kind().to_string(),
            timestamp: turn.timestamp.clone(),
            source: turn.source.clone(),
            message: turn.message.clone(),
            reasoning_content: turn.reasoning_content.clone(),
            reasoning_effort: turn.reasoning_effort.clone(),
            metrics: turn.metrics.clone(),
            model_name: turn.model_name.clone(),
            llm_call_count: turn.llm_call_count,
            is_copied_context: turn.is_copied_context,
            latency_ms: turn.latency_ms,
            ttft_ms: turn.ttft_ms,
            had_observation: turn.observation.is_some(),
            extra: turn.extra.clone(),
        });

        let mut calls = BTreeMap::new();
        for (call_index, call) in turn
            .tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if !seen_calls.insert(call.tool_call_id.clone()) {
                return Err(Error::DuplicateToolCall {
                    session_id: story.session_id.clone(),
                    tool_call_id: call.tool_call_id.clone(),
                });
            }
            calls.insert(
                call.tool_call_id.clone(),
                StoryToolCallRow {
                    document_id: document_id.clone(),
                    run_id: story.run_id.clone(),
                    session_id: story.session_id.clone(),
                    step_id: turn.id,
                    call_index: call_index as i64,
                    tool_call_id: call.tool_call_id.clone(),
                    function_name: call.function_name.clone(),
                    arguments: call.arguments.clone(),
                    result: call.result.clone(),
                    results: Vec::new(),
                    duration_ms: call.duration_ms,
                    extra: call.extra.clone(),
                },
            );
        }
        for result in observation_results(&turn.observation)? {
            let call_id = source_call_id(result).ok_or_else(|| {
                Error::Other(format!(
                    "observation result in step {} requires source_call_id",
                    turn.id
                ))
            })?;
            let call = calls
                .get_mut(call_id)
                .ok_or_else(|| Error::OrphanToolCall {
                    session_id: story.session_id.clone(),
                    step_id: turn.id,
                    tool_call_id: call_id.to_string(),
                })?;
            call.results.push(result.clone());
        }
        tool_calls.extend(calls.into_values());
    }
    steps.sort_by_key(|row| row.step_id);
    tool_calls.sort_by(|a, b| {
        a.step_id
            .cmp(&b.step_id)
            .then(a.call_index.cmp(&b.call_index))
    });
    Ok(StorylineTables {
        run,
        steps,
        tool_calls,
    })
}

pub fn reconstruct_storyline(tables: StorylineTables) -> Result<StorylineDocument> {
    let StorylineTables {
        run,
        mut steps,
        tool_calls,
    } = tables;
    let mut step_ids = HashSet::new();
    for step in &steps {
        if step.session_id != run.session_id
            || step.document_id != run.document_id
            || step.run_id != run.run_id
        {
            return Err(Error::Other(format!(
                "step {} does not belong to document/session {}/{}",
                step.step_id, run.document_id, run.session_id
            )));
        }
        if !step_ids.insert(step.step_id) {
            return Err(Error::DuplicateStep {
                session_id: run.session_id.clone(),
                step_id: step.step_id,
            });
        }
    }
    let mut call_ids = HashSet::new();
    for call in &tool_calls {
        if call.session_id != run.session_id
            || call.document_id != run.document_id
            || call.run_id != run.run_id
            || !step_ids.contains(&call.step_id)
        {
            return Err(Error::OrphanToolCall {
                session_id: run.session_id.clone(),
                step_id: call.step_id,
                tool_call_id: call.tool_call_id.clone(),
            });
        }
        if !call_ids.insert(call.tool_call_id.clone()) {
            return Err(Error::DuplicateToolCall {
                session_id: run.session_id.clone(),
                tool_call_id: call.tool_call_id.clone(),
            });
        }
        for result in &call.results {
            if source_call_id(result) != Some(call.tool_call_id.as_str()) {
                return Err(Error::Other(format!(
                    "result in tool_call '{}' has a mismatched source_call_id",
                    call.tool_call_id
                )));
            }
        }
    }
    steps.sort_by_key(|row| row.step_id);
    let mut calls_by_step: BTreeMap<i64, Vec<StoryToolCallRow>> = BTreeMap::new();
    for call in tool_calls {
        calls_by_step.entry(call.step_id).or_default().push(call);
    }
    let turns = steps
        .into_iter()
        .map(|step| {
            let mut calls = calls_by_step.remove(&step.step_id).unwrap_or_default();
            calls.sort_by_key(|call| call.call_index);
            let mut results = Vec::new();
            let tool_calls = calls
                .into_iter()
                .map(|call| {
                    results.extend(call.results);
                    StorylineToolCall {
                        tool_call_id: call.tool_call_id,
                        function_name: call.function_name,
                        arguments: call.arguments,
                        result: call.result,
                        duration_ms: call.duration_ms,
                        extra: call.extra,
                    }
                })
                .collect::<Vec<_>>();
            StorylineTurn {
                id: step.step_id,
                kind: step.kind,
                timestamp: step.timestamp,
                source: step.source,
                message: step.message,
                reasoning_content: step.reasoning_content,
                reasoning_effort: step.reasoning_effort,
                tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
                observation: step
                    .had_observation
                    .then_some(serde_json::json!({"results": results})),
                metrics: step.metrics,
                model_name: step.model_name,
                llm_call_count: step.llm_call_count,
                is_copied_context: step.is_copied_context,
                latency_ms: step.latency_ms,
                ttft_ms: step.ttft_ms,
                extra: step.extra,
            }
        })
        .collect();
    let presence = run.presence;
    let story = StorylineDocument {
        schema_version: run.schema_version,
        run_id: run.run_id,
        trajectory_id: run.trajectory_id_explicit.then_some(run.document_id),
        attempt_id: run.attempt_id,
        session_id: run.session_id,
        agent: crate::StorylineAgent {
            id: run.agent_id,
            name: run.agent_name,
            version: run.agent_version,
            model_name: run.agent_model_name,
            tool_definitions: run.agent_tool_definitions,
            extra: run.agent_extra,
        },
        parent: run.parent,
        child_session_ids: run.child_session_ids,
        notes: run.notes,
        final_metrics: run.final_metrics,
        continued_trajectory_ref: run.continued_trajectory_ref,
        extra: run.extra,
        presence,
        turns,
    };
    story.validate()?;
    Ok(story)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StorylineAgent;
    use serde_json::json;

    fn turn(id: i64, source: &str) -> StorylineTurn {
        StorylineTurn {
            id,
            kind: None,
            timestamp: None,
            source: source.into(),
            message: json!(format!("message-{id}")),
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
        }
    }

    fn story() -> StorylineDocument {
        let mut tool_turn = turn(2, "agent");
        tool_turn.tool_calls = Some(vec![StorylineToolCall {
            tool_call_id: "call-1".into(),
            function_name: "lookup".into(),
            arguments: json!({"query": "answer"}),
            result: FieldPresence::Missing,
            duration_ms: Some(7),
            extra: Some(json!({"provider": "test"})),
        }]);
        tool_turn.observation = Some(json!({
            "results": [{"source_call_id": "call-1", "content": "42"}]
        }));
        StorylineDocument {
            schema_version: None,
            run_id: Some("run-1".into()),
            trajectory_id: None,
            attempt_id: None,
            session_id: "session-1".into(),
            agent: StorylineAgent {
                id: "agent-1".into(),
                name: Some("Agent".into()),
                version: Some("1".into()),
                model_name: Some("model".into()),
                tool_definitions: Some(json!([{"name": "lookup"}])),
                extra: Some(json!({"team": "tests"})),
            },
            parent: None,
            child_session_ids: None,
            notes: Some("notes".into()),
            final_metrics: Some(json!({"score": 1})),
            continued_trajectory_ref: None,
            extra: Some(json!({"case": "roundtrip"})),
            presence: Default::default(),
            turns: vec![turn(1, "user"), tool_turn],
        }
    }

    #[test]
    fn three_table_roundtrip_preserves_run_and_observation_semantics() {
        let mut expected = story();
        expected.schema_version = Some("ATIF-v1.7".into());
        expected.attempt_id = Some("attempt-1".into());
        expected.turns[1].tool_calls.as_mut().unwrap()[0].result = crate::FieldPresence::Null;
        let tables = split_storyline(&expected).unwrap();
        assert!(!tables.run.trajectory_id_explicit);
        assert_eq!(tables.run.run_id.as_deref(), Some("run-1"));
        assert_eq!(tables.run.document_id, "session-1");
        assert_eq!(tables.steps.len(), 2);
        assert_eq!(tables.tool_calls.len(), 1);
        assert_eq!(tables.tool_calls[0].results.len(), 1);
        assert_eq!(tables.tool_calls[0].result, crate::FieldPresence::Null);
        assert_eq!(reconstruct_storyline(tables).unwrap(), expected);

        let mut implicit_run = story();
        implicit_run.run_id = None;
        implicit_run.turns[0].observation = Some(json!({"results": []}));
        let tables = split_storyline(&implicit_run).unwrap();
        assert!(!tables.run.trajectory_id_explicit);
        assert_eq!(tables.run.run_id, None);
        assert_eq!(tables.run.document_id, implicit_run.session_id);
        assert!(tables.steps[0].had_observation);
        assert_eq!(reconstruct_storyline(tables).unwrap(), implicit_run);
    }

    #[test]
    fn split_rejects_invalid_result_correlations() {
        let mut missing_results = story();
        missing_results.turns[1].observation = Some(json!({}));
        assert!(split_storyline(&missing_results)
            .unwrap_err()
            .to_string()
            .contains("results array"));

        let mut missing_id = story();
        missing_id.turns[1].observation = Some(json!({"results": [{"content": "42"}]}));
        assert!(split_storyline(&missing_id)
            .unwrap_err()
            .to_string()
            .contains("requires source_call_id"));

        let mut orphan = story();
        orphan.turns[1].observation = Some(json!({"results": [{"source_call_id": "missing"}]}));
        assert!(matches!(
            split_storyline(&orphan),
            Err(Error::OrphanToolCall { .. })
        ));

        let mut duplicate = story();
        let mut second = duplicate.turns[1].clone();
        second.id = 3;
        second.observation = None;
        duplicate.turns.push(second);
        assert!(matches!(
            split_storyline(&duplicate),
            Err(Error::DuplicateToolCall { .. })
        ));
    }

    #[test]
    fn reconstruction_enforces_three_table_foreign_keys_and_uniqueness() {
        let valid = split_storyline(&story()).unwrap();

        let mut wrong_step_owner = valid.clone();
        wrong_step_owner.steps[0].session_id = "other".into();
        assert!(reconstruct_storyline(wrong_step_owner).is_err());

        let mut duplicate_step = valid.clone();
        duplicate_step.steps.push(duplicate_step.steps[0].clone());
        assert!(matches!(
            reconstruct_storyline(duplicate_step),
            Err(Error::DuplicateStep { .. })
        ));

        let mut orphan_call = valid.clone();
        orphan_call.tool_calls[0].step_id = 99;
        assert!(matches!(
            reconstruct_storyline(orphan_call),
            Err(Error::OrphanToolCall { .. })
        ));

        let mut duplicate_call = valid.clone();
        duplicate_call
            .tool_calls
            .push(duplicate_call.tool_calls[0].clone());
        assert!(matches!(
            reconstruct_storyline(duplicate_call),
            Err(Error::DuplicateToolCall { .. })
        ));

        let mut mismatched_result = valid;
        mismatched_result.tool_calls[0].results[0]["source_call_id"] = json!("other-call");
        assert!(reconstruct_storyline(mismatched_result)
            .unwrap_err()
            .to_string()
            .contains("mismatched source_call_id"));
    }
}
