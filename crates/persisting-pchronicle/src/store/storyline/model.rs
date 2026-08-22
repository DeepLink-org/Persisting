//! Relational rows for the Storyline-native, three-table Lance projection.
//!
//! A Storyline document is normalized into one run row, ordered step rows, and
//! tool-call rows. ATIF-compatible `observation.results[]` values are attached
//! to their call through `source_call_id` and stored in `results`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::formats::unknown_fields::{
    validate_unknown_fields, StorylineUnknownFields, UnknownFieldLimits, UnknownKeyCounts,
};
use crate::model::{
    StorylineEnv, StorylineOrigin, StorylinePrompt, StorylineTask, StorylineTimestamp,
    StorylineToolResponse,
};
use crate::{Result, StoryLink, StorylineDocument, StorylineToolCall, StorylineTurn};

#[cfg(feature = "lance-store")]
pub const STORY_RUNS_TABLE: &str = "runs";
#[cfg(feature = "lance-store")]
pub const STORY_STEPS_TABLE: &str = "steps";
#[cfg(feature = "lance-store")]
pub const STORY_TOOL_CALLS_TABLE: &str = "tool_calls";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryRunRow {
    pub schema_version: String,
    pub origin: Option<StorylineOrigin>,
    /// Stable per-document storage identity. Explicit ATIF `trajectory_id`
    /// wins; otherwise the effective `session_id` is used.
    pub document_id: String,
    /// Stable global order inside this Storyline store.
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
    pub task: Option<StorylineTask>,
    pub prompt: Option<StorylinePrompt>,
    pub started_at: Option<StorylineTimestamp>,
    pub finished_at: Option<StorylineTimestamp>,
    pub final_metrics: Option<Value>,
    pub continued_trajectory_ref: Option<String>,
    pub extra: Option<Value>,
    pub meta: Option<Value>,
    pub unknown_fields: StorylineUnknownFields,
    pub unknown_key_counts: UnknownKeyCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryStepRow {
    pub document_id: String,
    pub run_id: Option<String>,
    pub session_id: String,
    pub step_id: i64,
    pub turn_ordinal: i64,
    pub kind: Option<String>,
    pub effective_kind: String,
    pub timestamp: Option<crate::model::StorylineTimestamp>,
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
    /// Keeps `tool_calls: []` distinct from no `tool_calls` member.
    pub had_tool_calls: bool,
    /// Keeps `observation: {"results": []}` distinct from no observation.
    pub had_observation: bool,
    /// Complete authoritative observation. `StoryToolCallRow::results` is derived.
    pub observation: Option<Value>,
    pub extra: Option<Value>,
    pub env: Option<StorylineEnv>,
    pub prompt: Option<StorylinePrompt>,
    pub finished_at: Option<StorylineTimestamp>,
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
    pub result: Option<Value>,
    pub results: Vec<Value>,
    pub duration_ms: Option<i64>,
    pub extra: Option<Value>,
    pub kind: Option<String>,
    pub response: Option<StorylineToolResponse>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StorylineTables {
    pub run: StoryRunRow,
    pub steps: Vec<StoryStepRow>,
    pub tool_calls: Vec<StoryToolCallRow>,
}

fn observation_results(observation: Option<&Value>) -> Option<&[Value]> {
    let observation = observation?;
    observation
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
}

fn source_call_id(result: &Value) -> Option<&str> {
    result
        .get("source_call_id")
        .or_else(|| result.get("tool_call_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

pub fn split_storyline(story: &StorylineDocument) -> Result<StorylineTables> {
    split_storyline_with_unknown_limits(story, UnknownFieldLimits::default())
}

pub(crate) fn split_storyline_with_unknown_limits(
    story: &StorylineDocument,
    unknown_limits: UnknownFieldLimits,
) -> Result<StorylineTables> {
    story.validate()?;
    let counts = validate_unknown_fields(&story.unknown_fields, unknown_limits)?;
    anyhow::ensure!(
        counts == story.unknown_key_counts,
        "storyline unknown_key_counts do not match unknown_fields"
    );
    let document_id = story.document_id().to_string();
    let run = StoryRunRow {
        schema_version: story.schema_version.clone(),
        origin: story.origin.clone(),
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
        task: story.task.clone(),
        prompt: story.prompt.clone(),
        started_at: story.started_at.clone(),
        finished_at: story.finished_at.clone(),
        final_metrics: story.final_metrics.clone(),
        continued_trajectory_ref: story.continued_trajectory_ref.clone(),
        extra: story.extra.clone(),
        meta: story.meta.clone(),
        unknown_fields: story.unknown_fields.clone(),
        unknown_key_counts: story.unknown_key_counts.clone(),
    };

    let mut seen_calls = HashSet::new();
    let mut steps = Vec::with_capacity(story.turns.len());
    let mut tool_calls = Vec::new();
    for (turn_ordinal, turn) in story.turns.iter().enumerate() {
        steps.push(StoryStepRow {
            document_id: document_id.clone(),
            run_id: story.run_id.clone(),
            session_id: story.session_id.clone(),
            step_id: turn.id,
            turn_ordinal: i64::try_from(turn_ordinal)
                .map_err(|_| anyhow::anyhow!("storyline turn ordinal overflow"))?,
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
            had_tool_calls: turn.tool_calls.is_some(),
            had_observation: turn.observation.is_some(),
            observation: turn.observation.clone(),
            extra: turn.extra.clone(),
            env: turn.env.clone(),
            prompt: turn.prompt.clone(),
            finished_at: turn.finished_at.clone(),
        });

        let mut call_positions = HashMap::new();
        for (call_index, call) in turn
            .tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            if !seen_calls.insert(call.tool_call_id.clone()) {
                anyhow::bail!(
                    "duplicate tool_call ({}, {})",
                    story.session_id,
                    call.tool_call_id
                );
            }
            let position = tool_calls.len();
            call_positions.insert(call.tool_call_id.as_str(), position);
            tool_calls.push(StoryToolCallRow {
                document_id: document_id.clone(),
                run_id: story.run_id.clone(),
                session_id: story.session_id.clone(),
                step_id: turn.id,
                call_index: i64::try_from(call_index)
                    .map_err(|_| anyhow::anyhow!("storyline tool-call index overflow"))?,
                tool_call_id: call.tool_call_id.clone(),
                function_name: call.function_name.clone(),
                arguments: call.arguments.clone(),
                result: call.result.clone(),
                results: Vec::new(),
                duration_ms: call.duration_ms,
                extra: call.extra.clone(),
                kind: call.kind.clone(),
                response: call.response.clone(),
            });
        }
        for result in observation_results(turn.observation.as_ref()).unwrap_or_default() {
            let Some(call_id) = source_call_id(result) else {
                continue;
            };
            let Some(position) = call_positions.get(call_id) else {
                continue;
            };
            tool_calls[*position].results.push(result.clone());
        }
    }
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
    let steps_with_tool_calls = tool_calls
        .iter()
        .map(|call| call.step_id)
        .collect::<HashSet<_>>();
    let mut step_ids = HashSet::new();
    let mut turn_ordinals = HashSet::new();
    for step in &steps {
        if step.session_id != run.session_id
            || step.document_id != run.document_id
            || step.run_id != run.run_id
        {
            anyhow::bail!(
                "step {} does not belong to document/session {}/{}",
                step.step_id,
                run.document_id,
                run.session_id
            );
        }
        if !step_ids.insert(step.step_id) {
            anyhow::bail!("duplicate step ({}, {})", run.session_id, step.step_id);
        }
        if step.turn_ordinal < 0 || !turn_ordinals.insert(step.turn_ordinal) {
            anyhow::bail!(
                "invalid or duplicate turn ordinal {} in session {}",
                step.turn_ordinal,
                run.session_id
            );
        }
        if step.had_observation != step.observation.is_some() {
            anyhow::bail!(
                "step {} observation presence does not match observation_json",
                step.step_id
            );
        }
        if !step.had_tool_calls && steps_with_tool_calls.contains(&step.step_id) {
            anyhow::bail!(
                "step {} has tool-call rows but tool_calls was absent",
                step.step_id
            );
        }
    }
    let step_count = i64::try_from(steps.len())
        .map_err(|_| anyhow::anyhow!("storyline step count exceeds i64"))?;
    anyhow::ensure!(
        (0..step_count).all(|ordinal| turn_ordinals.contains(&ordinal)),
        "turn ordinals must be contiguous from zero in session {}",
        run.session_id
    );
    let observations_by_step = steps
        .iter()
        .map(|step| (step.step_id, step.observation.as_ref()))
        .collect::<HashMap<_, _>>();
    let mut call_ids = HashSet::new();
    let mut call_indices_by_step = HashMap::<i64, HashSet<i64>>::new();
    for call in &tool_calls {
        if call.session_id != run.session_id
            || call.document_id != run.document_id
            || call.run_id != run.run_id
            || !step_ids.contains(&call.step_id)
        {
            anyhow::bail!(
                "tool_call {} references missing step {} in session {}",
                call.tool_call_id,
                call.step_id,
                run.session_id
            );
        }
        if !call_ids.insert(call.tool_call_id.clone()) {
            anyhow::bail!(
                "duplicate tool_call ({}, {})",
                run.session_id,
                call.tool_call_id
            );
        }
        let call_indices = call_indices_by_step.entry(call.step_id).or_default();
        if call.call_index < 0 || !call_indices.insert(call.call_index) {
            anyhow::bail!(
                "invalid or duplicate call index {} for step {}",
                call.call_index,
                call.step_id
            );
        }
        for result in &call.results {
            if source_call_id(result) != Some(call.tool_call_id.as_str()) {
                anyhow::bail!(
                    "result in tool_call '{}' has a mismatched source_call_id",
                    call.tool_call_id
                );
            }
        }
        let expected_results =
            observation_results(observations_by_step.get(&call.step_id).copied().flatten())
                .unwrap_or_default()
                .iter()
                .filter(|result| source_call_id(result) == Some(call.tool_call_id.as_str()))
                .cloned()
                .collect::<Vec<_>>();
        anyhow::ensure!(
            call.results == expected_results,
            "derived results for tool_call '{}' do not match observation_json",
            call.tool_call_id
        );
    }
    for (step_id, indices) in &call_indices_by_step {
        let call_count = i64::try_from(indices.len())
            .map_err(|_| anyhow::anyhow!("storyline tool-call count exceeds i64"))?;
        anyhow::ensure!(
            (0..call_count).all(|index| indices.contains(&index)),
            "call indexes must be contiguous from zero for step {step_id}"
        );
    }
    drop(observations_by_step);
    steps.sort_by_key(|row| row.turn_ordinal);
    let mut calls_by_step: BTreeMap<i64, Vec<StoryToolCallRow>> = BTreeMap::new();
    for call in tool_calls {
        calls_by_step.entry(call.step_id).or_default().push(call);
    }
    let turns = steps
        .into_iter()
        .map(|step| {
            let mut calls = calls_by_step.remove(&step.step_id).unwrap_or_default();
            calls.sort_by_key(|call| call.call_index);
            let tool_calls = calls
                .into_iter()
                .map(|call| StorylineToolCall {
                    tool_call_id: call.tool_call_id,
                    function_name: call.function_name,
                    arguments: call.arguments,
                    result: call.result,
                    duration_ms: call.duration_ms,
                    extra: call.extra,
                    kind: call.kind,
                    response: call.response,
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
                tool_calls: step.had_tool_calls.then_some(tool_calls),
                observation: step.observation,
                metrics: step.metrics,
                model_name: step.model_name,
                llm_call_count: step.llm_call_count,
                is_copied_context: step.is_copied_context,
                latency_ms: step.latency_ms,
                ttft_ms: step.ttft_ms,
                extra: step.extra,
                env: step.env,
                prompt: step.prompt,
                finished_at: step.finished_at,
            }
        })
        .collect();
    let story = StorylineDocument {
        schema_version: run.schema_version,
        origin: run.origin,
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
        task: run.task,
        prompt: run.prompt,
        started_at: run.started_at,
        finished_at: run.finished_at,
        final_metrics: run.final_metrics,
        continued_trajectory_ref: run.continued_trajectory_ref,
        extra: run.extra,
        meta: run.meta,
        unknown_fields: run.unknown_fields,
        unknown_key_counts: run.unknown_key_counts,
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
            env: None,
            prompt: None,
            finished_at: None,
        }
    }

    fn story() -> StorylineDocument {
        let mut tool_turn = turn(2, "agent");
        tool_turn.tool_calls = Some(vec![StorylineToolCall {
            tool_call_id: "call-1".into(),
            function_name: "lookup".into(),
            arguments: json!({"query": "answer"}),
            result: None,
            duration_ms: Some(7),
            extra: Some(json!({"provider": "test"})),
            kind: None,
            response: None,
        }]);
        tool_turn.observation = Some(json!({
            "results": [{"source_call_id": "call-1", "content": "42"}]
        }));
        StorylineDocument {
            schema_version: crate::model::STORYLINE_SCHEMA_VERSION.into(),
            origin: None,
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
            task: None,
            prompt: None,
            started_at: None,
            finished_at: None,
            final_metrics: Some(json!({"score": 1})),
            continued_trajectory_ref: None,
            extra: Some(json!({"case": "roundtrip"})),
            meta: Some(json!({"suite": "roundtrip"})),
            unknown_fields: Default::default(),
            unknown_key_counts: Default::default(),
            turns: vec![turn(1, "user"), tool_turn],
        }
    }

    #[test]
    fn three_table_roundtrip_preserves_run_and_observation_semantics() {
        let mut expected = story();
        expected.origin = Some(crate::model::StorylineOrigin {
            format: crate::format::DocumentFormat::Atif.as_str().into(),
            schema_version: Some("ATIF-v1.7".into()),
            document_id: None,
        });
        expected.attempt_id = Some("attempt-1".into());
        expected
            .unknown_fields
            .insert("atif", "source-1", "/vendor", json!(7))
            .unwrap();
        expected.refresh_unknown_key_counts().unwrap();
        expected.turns[1].tool_calls.as_mut().unwrap()[0].result = None;
        let tables = split_storyline(&expected).unwrap();
        assert!(!tables.run.trajectory_id_explicit);
        assert_eq!(tables.run.run_id.as_deref(), Some("run-1"));
        assert_eq!(tables.run.document_id, "session-1");
        assert_eq!(tables.steps.len(), 2);
        assert_eq!(tables.tool_calls.len(), 1);
        assert_eq!(tables.tool_calls[0].results.len(), 1);
        assert_eq!(tables.tool_calls[0].result, None);
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
    fn three_table_roundtrip_preserves_turn_order_presence_and_raw_observation() {
        let mut story = StorylineDocument::new("session-order", "agent");
        let mut first = turn(9, "user");
        first.tool_calls = Some(Vec::new());

        let mut second = turn(3, "agent");
        second.tool_calls = Some(vec![
            StorylineToolCall {
                tool_call_id: "call-b".into(),
                function_name: "second".into(),
                arguments: json!({"n": 2}),
                result: None,
                duration_ms: None,
                extra: None,
                kind: None,
                response: None,
            },
            StorylineToolCall {
                tool_call_id: "call-a".into(),
                function_name: "first".into(),
                arguments: json!({"n": 1}),
                result: None,
                duration_ms: None,
                extra: None,
                kind: None,
                response: None,
            },
        ]);
        second.observation = Some(json!({
            "vendor": {"trace": 7},
            "results": [
                {"source_call_id": "call-b", "content": "b-1"},
                {"source_call_id": "call-a", "content": "a-1"},
                {"source_call_id": "call-b", "content": "b-2"}
            ]
        }));
        story.turns = vec![first, second];

        let reconstructed = reconstruct_storyline(split_storyline(&story).unwrap()).unwrap();

        assert_eq!(reconstructed, story);
    }

    #[test]
    fn split_accepts_arbitrary_observations_without_changing_them() {
        for observation in [
            json!({}),
            json!({"results": [{"content": "42"}]}),
            json!({"results": [{"source_call_id": "missing"}]}),
            json!([1, null, {"provider": true}]),
        ] {
            let mut document = story();
            document.turns[1].observation = Some(observation);
            let roundtrip = reconstruct_storyline(split_storyline(&document).unwrap()).unwrap();
            assert_eq!(roundtrip, document);
        }
    }

    #[test]
    fn split_rejects_duplicate_tool_call_ids() {
        let mut duplicate = story();
        let mut second = duplicate.turns[1].clone();
        second.id = 3;
        second.observation = None;
        duplicate.turns.push(second);
        assert!(split_storyline(&duplicate).is_err());
    }

    #[test]
    fn reconstruction_enforces_three_table_foreign_keys_and_uniqueness() {
        let valid = split_storyline(&story()).unwrap();

        let mut wrong_step_owner = valid.clone();
        wrong_step_owner.steps[0].session_id = "other".into();
        assert!(reconstruct_storyline(wrong_step_owner).is_err());

        let mut duplicate_step = valid.clone();
        duplicate_step.steps.push(duplicate_step.steps[0].clone());
        assert!(reconstruct_storyline(duplicate_step).is_err());

        let mut duplicate_ordinal = valid.clone();
        duplicate_ordinal.steps[1].turn_ordinal = duplicate_ordinal.steps[0].turn_ordinal;
        assert!(reconstruct_storyline(duplicate_ordinal).is_err());

        let mut gapped_ordinal = valid.clone();
        gapped_ordinal.steps[1].turn_ordinal = 7;
        assert!(reconstruct_storyline(gapped_ordinal)
            .unwrap_err()
            .to_string()
            .contains("turn ordinals must be contiguous"));

        let mut orphan_call = valid.clone();
        orphan_call.tool_calls[0].step_id = 99;
        assert!(reconstruct_storyline(orphan_call).is_err());

        let mut duplicate_call = valid.clone();
        duplicate_call
            .tool_calls
            .push(duplicate_call.tool_calls[0].clone());
        assert!(reconstruct_storyline(duplicate_call).is_err());

        let mut gapped_call_index = valid.clone();
        gapped_call_index.tool_calls[0].call_index = 1;
        assert!(reconstruct_storyline(gapped_call_index)
            .unwrap_err()
            .to_string()
            .contains("call indexes must be contiguous"));

        let mut duplicate_call_index = valid.clone();
        let mut second_call = duplicate_call_index.tool_calls[0].clone();
        second_call.tool_call_id = "call-2".into();
        second_call.results.clear();
        duplicate_call_index.tool_calls.push(second_call);
        assert!(reconstruct_storyline(duplicate_call_index)
            .unwrap_err()
            .to_string()
            .contains("duplicate call index"));

        let mut stale_derived_results = valid.clone();
        stale_derived_results.tool_calls[0].results.clear();
        assert!(reconstruct_storyline(stale_derived_results)
            .unwrap_err()
            .to_string()
            .contains("do not match observation_json"));

        let mut mismatched_result = valid;
        mismatched_result.tool_calls[0].results[0]["source_call_id"] = json!("other-call");
        assert!(reconstruct_storyline(mismatched_result)
            .unwrap_err()
            .to_string()
            .contains("mismatched source_call_id"));
    }

    #[test]
    fn split_storyline_validates_configured_unknown_field_limit() {
        let mut oversized = story();
        oversized
            .unknown_fields
            .insert("atif", "source", "/one", json!(1))
            .unwrap();
        oversized
            .unknown_fields
            .insert("atif", "source", "/two", json!(2))
            .unwrap();
        oversized.refresh_unknown_key_counts().unwrap();

        let error = split_storyline_with_unknown_limits(
            &oversized,
            UnknownFieldLimits {
                max_fields: 1,
                max_bytes: 1024,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown field count 2 exceeds configured limit 1"),
            "{error:#}"
        );
    }
}
