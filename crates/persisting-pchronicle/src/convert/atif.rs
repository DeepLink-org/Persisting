//! ATIF ⇄ storyline.

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::formats::storyline::{
    FieldPresence, PresenceState, StoryLink, StorylineAgent, StorylineAgentField,
    StorylineCollectionShape, StorylineDocument, StorylinePresence, StorylineRootField,
    StorylineToolCall, StorylineTurn, StorylineTurnField,
};
use crate::{DocumentFormat, Error, Result};

fn timing_from_metrics(metrics: &FieldPresence<serde_json::Value>) -> (Option<i64>, Option<i64>) {
    let Some(m) = metrics.value() else {
        return (None, None);
    };
    let latency = m
        .get("latency_ms")
        .or_else(|| m.get("elapsed_ms"))
        .or_else(|| m.get("duration_ms"))
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
    let ttft = m
        .get("ttft_ms")
        .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
    (latency, ttft)
}

trait NullField {
    fn is_null_field(&self) -> bool;
}

impl<T> NullField for FieldPresence<T> {
    fn is_null_field(&self) -> bool {
        self.is_null()
    }
}

fn presence_state<T>(field: &FieldPresence<T>) -> PresenceState {
    match field {
        FieldPresence::Missing => PresenceState::Missing,
        FieldPresence::Null => PresenceState::Null,
        FieldPresence::Value(_) => PresenceState::Value,
    }
}

fn mark_root_null<T>(
    presence: &mut StorylinePresence,
    field: StorylineRootField,
    value: &FieldPresence<T>,
) {
    if value.is_null() {
        presence.root_nulls.insert(field);
    }
}

fn mark_agent_null<T>(
    presence: &mut StorylinePresence,
    field: StorylineAgentField,
    value: &FieldPresence<T>,
) {
    if value.is_null() {
        presence.agent_nulls.insert(field);
    }
}

fn field_from_option<T>(value: Option<T>, explicit_null: bool) -> FieldPresence<T> {
    match value {
        Some(value) => FieldPresence::Value(value),
        None if explicit_null => FieldPresence::Null,
        None => FieldPresence::Missing,
    }
}

#[cfg(test)]
pub fn atif_to_storyline(traj: &AtifTrajectory) -> Result<StorylineDocument> {
    if traj
        .subagent_trajectories
        .value()
        .is_some_and(|children| !children.is_empty())
    {
        return Err(Error::Other(
            "embedded ATIF subagent trajectories require atif_to_storylines".into(),
        ));
    }
    atif_to_storyline_node(traj, None, None)
}

/// Flatten one ATIF tree into authoritative Storyline documents.
///
/// Embedded subagents retain input order through each parent's `children`
/// list. Missing child `session_id` values inherit the parent's effective
/// storage identity while their original presence remains explicit.
pub fn atif_to_storylines(traj: &AtifTrajectory) -> Result<Vec<StorylineDocument>> {
    fn visit(
        trajectory: &AtifTrajectory,
        parent_key: Option<&str>,
        inherited_session_id: Option<&str>,
        output: &mut Vec<StorylineDocument>,
    ) -> Result<()> {
        let story = atif_to_storyline_node(trajectory, parent_key, inherited_session_id)?;
        let parent_key = story
            .trajectory_id
            .as_deref()
            .unwrap_or(story.session_id.as_str())
            .to_string();
        let inherited_session_id = story.session_id.clone();
        output.push(story);
        if let Some(children) = trajectory.subagent_trajectories.value() {
            for child in children {
                visit(
                    child,
                    Some(&parent_key),
                    Some(&inherited_session_id),
                    output,
                )?;
            }
        }
        Ok(())
    }

    let mut output = Vec::new();
    visit(traj, None, None, &mut output)?;
    Ok(output)
}

/// Convert one top-level ATIF trajectory and attach its format-neutral
/// collection semantics to every flattened Storyline node.
pub(crate) fn atif_collection_to_storylines(
    traj: &AtifTrajectory,
    shape: StorylineCollectionShape,
    ordinal: i64,
) -> Result<Vec<StorylineDocument>> {
    if ordinal < 0 {
        return Err(Error::Other(
            "ATIF collection ordinal cannot be negative".into(),
        ));
    }
    let mut stories = atif_to_storylines(traj)?;
    for story in &mut stories {
        story.presence.collection_shape = Some(shape);
        story.presence.collection_ordinal = Some(ordinal);
    }
    Ok(stories)
}

fn atif_to_storyline_node(
    traj: &AtifTrajectory,
    parent_key: Option<&str>,
    inherited_session_id: Option<&str>,
) -> Result<StorylineDocument> {
    let session_id = traj
        .session_id
        .value()
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .or(inherited_session_id)
        .or_else(|| {
            traj.trajectory_id
                .value()
                .map(String::as_str)
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| {
            Error::InvalidAtif("ATIF trajectory requires an effective storage identity".into())
        })?
        .to_string();
    let child_ids = traj.subagent_trajectories.value().map(|children| {
        children
            .iter()
            .map(|child| {
                child
                    .trajectory_id
                    .value()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        Error::InvalidAtif("embedded ATIF subagent requires trajectory_id".into())
                    })
                    .cloned()
            })
            .collect::<Result<Vec<_>>>()
    });
    let child_ids = child_ids.transpose()?;

    let mut presence = StorylinePresence {
        session_id: presence_state(&traj.session_id),
        ..StorylinePresence::default()
    };
    mark_root_null(
        &mut presence,
        StorylineRootField::TrajectoryId,
        &traj.trajectory_id,
    );
    mark_root_null(&mut presence, StorylineRootField::Notes, &traj.notes);
    mark_root_null(
        &mut presence,
        StorylineRootField::FinalMetrics,
        &traj.final_metrics,
    );
    mark_root_null(
        &mut presence,
        StorylineRootField::ContinuedTrajectoryRef,
        &traj.continued_trajectory_ref,
    );
    mark_root_null(&mut presence, StorylineRootField::Extra, &traj.extra);
    mark_root_null(
        &mut presence,
        StorylineRootField::SubagentTrajectories,
        &traj.subagent_trajectories,
    );
    mark_agent_null(
        &mut presence,
        StorylineAgentField::ModelName,
        &traj.agent.model_name,
    );
    mark_agent_null(
        &mut presence,
        StorylineAgentField::ToolDefinitions,
        &traj.agent.tool_definitions,
    );
    mark_agent_null(&mut presence, StorylineAgentField::Extra, &traj.agent.extra);

    let mut turns = Vec::new();
    for step in &traj.steps {
        for (field, value) in [
            (
                StorylineTurnField::Timestamp,
                &step.timestamp as &dyn NullField,
            ),
            (StorylineTurnField::ModelName, &step.model_name),
            (StorylineTurnField::ReasoningEffort, &step.reasoning_effort),
            (
                StorylineTurnField::ReasoningContent,
                &step.reasoning_content,
            ),
            (StorylineTurnField::ToolCalls, &step.tool_calls),
            (StorylineTurnField::Observation, &step.observation),
            (StorylineTurnField::Metrics, &step.metrics),
            (StorylineTurnField::Extra, &step.extra),
            (StorylineTurnField::LlmCallCount, &step.llm_call_count),
            (StorylineTurnField::IsCopiedContext, &step.is_copied_context),
        ] {
            if value.is_null_field() {
                presence
                    .turn_nulls
                    .entry(step.step_id)
                    .or_default()
                    .insert(field);
            }
        }

        let tool_calls = step.tool_calls.value().map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let duration_ms = c
                        .extra
                        .value()
                        .and_then(|x| x.get("duration_ms"))
                        .and_then(|v| v.as_i64());
                    if c.extra.is_null() {
                        presence
                            .tool_call_extra_nulls
                            .insert(c.tool_call_id.clone());
                    }
                    StorylineToolCall {
                        tool_call_id: c.tool_call_id.clone(),
                        function_name: c.function_name.clone(),
                        arguments: c.arguments.clone(),
                        result: c.result.clone(),
                        duration_ms,
                        extra: c.extra.clone().into_option(),
                    }
                })
                .collect::<Vec<_>>()
        });

        let (latency_ms, ttft_ms) = timing_from_metrics(&step.metrics);

        let mut turn = StorylineTurn {
            id: step.step_id,
            kind: None,
            timestamp: step.timestamp.clone().into_option(),
            source: step.source.clone(),
            message: step.message.clone(),
            reasoning_content: step.reasoning_content.clone().into_option(),
            reasoning_effort: step.reasoning_effort.clone().into_option(),
            tool_calls,
            observation: step
                .observation
                .value()
                .map(serde_json::to_value)
                .transpose()?,
            metrics: step.metrics.clone().into_option(),
            model_name: step.model_name.clone().into_option(),
            llm_call_count: step.llm_call_count.clone().into_option(),
            is_copied_context: step.is_copied_context.clone().into_option(),
            latency_ms,
            ttft_ms,
            extra: step.extra.clone().into_option(),
        };
        let derived = turn.effective_kind().to_string();
        if !matches!(
            (step.source.as_str(), derived.as_str()),
            ("user", "dialogue") | ("system", "internal") | ("agent", "dialogue")
        ) {
            turn.kind = Some(derived);
        }
        turns.push(turn);
    }

    Ok(StorylineDocument {
        schema_version: Some(traj.schema_version.clone()),
        run_id: None,
        trajectory_id: traj.trajectory_id.clone().into_option(),
        attempt_id: None,
        session_id,
        agent: StorylineAgent {
            id: traj.agent.name.clone(),
            name: Some(traj.agent.name.clone()),
            version: Some(traj.agent.version.clone()),
            model_name: traj.agent.model_name.clone().into_option(),
            tool_definitions: traj.agent.tool_definitions.clone().into_option(),
            extra: traj.agent.extra.clone().into_option(),
        },
        parent: parent_key.map(|parent_session_id| StoryLink {
            parent_session_id: parent_session_id.to_string(),
            spawn_call_id: None,
            spawn_id: None,
            relation: "spawn".into(),
        }),
        child_session_ids: child_ids,
        notes: traj.notes.clone().into_option(),
        final_metrics: traj.final_metrics.clone().into_option(),
        continued_trajectory_ref: traj.continued_trajectory_ref.clone().into_option(),
        extra: traj.extra.clone().into_option(),
        presence,
        turns,
    })
}

#[cfg(test)]
pub fn storyline_to_atif(story: &StorylineDocument) -> Result<AtifTrajectory> {
    if story
        .child_session_ids
        .as_ref()
        .is_some_and(|children| !children.is_empty())
    {
        return Err(Error::Other(
            "Storyline with child documents requires storylines_to_atif".into(),
        ));
    }
    storyline_to_atif_node(story, None)
}

fn storyline_to_atif_node(
    story: &StorylineDocument,
    embedded_children: Option<Vec<AtifTrajectory>>,
) -> Result<AtifTrajectory> {
    story.validate()?;
    let mut steps = Vec::new();
    for (step_index, turn) in story.turns.iter().enumerate() {
        let observation = turn
            .observation
            .as_ref()
            .map(|value| {
                serde_json::from_value::<AtifObservation>(value.clone()).map_err(|error| {
                    Error::InvalidDocument {
                        format: DocumentFormat::Atif,
                        path: None,
                        location: Some(format!("step[{step_index}].observation")),
                        message: error.to_string(),
                    }
                })
            })
            .transpose()?;

        let tool_calls = turn.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let mut extra = c.extra.clone().unwrap_or(serde_json::json!({}));
                    if let Some(ms) = c.duration_ms {
                        if let Some(obj) = extra.as_object_mut() {
                            obj.insert("duration_ms".into(), serde_json::json!(ms));
                        }
                    }
                    let extra = if story
                        .presence
                        .tool_call_extra_nulls
                        .contains(&c.tool_call_id)
                        && extra.as_object().is_some_and(|object| object.is_empty())
                    {
                        FieldPresence::Null
                    } else if extra.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                        FieldPresence::Missing
                    } else {
                        FieldPresence::Value(extra)
                    };
                    AtifToolCall {
                        tool_call_id: c.tool_call_id.clone(),
                        function_name: c.function_name.clone(),
                        arguments: c.arguments.clone(),
                        result: c.result.clone(),
                        extra,
                    }
                })
                .collect()
        });

        let mut metrics = turn.metrics.clone().unwrap_or(serde_json::json!({}));
        if let Some(obj) = metrics.as_object_mut() {
            if let Some(ms) = turn.latency_ms {
                obj.entry("latency_ms".to_string())
                    .or_insert(serde_json::json!(ms));
            }
            if let Some(ms) = turn.ttft_ms {
                obj.entry("ttft_ms".to_string())
                    .or_insert(serde_json::json!(ms));
            }
        }
        let metrics = if turn.metrics.is_none()
            && story
                .presence
                .turn_nulls
                .get(&turn.id)
                .is_some_and(|fields| fields.contains(&StorylineTurnField::Metrics))
        {
            FieldPresence::Null
        } else if metrics.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            FieldPresence::Missing
        } else {
            FieldPresence::Value(metrics)
        };

        let turn_null = |field| {
            story
                .presence
                .turn_nulls
                .get(&turn.id)
                .is_some_and(|fields| fields.contains(&field))
        };

        steps.push(AtifStep {
            step_id: turn.id,
            timestamp: field_from_option(
                turn.timestamp.clone(),
                turn_null(StorylineTurnField::Timestamp),
            ),
            source: turn.source.clone(),
            model_name: field_from_option(
                turn.model_name.clone(),
                turn_null(StorylineTurnField::ModelName),
            ),
            reasoning_effort: field_from_option(
                turn.reasoning_effort.clone(),
                turn_null(StorylineTurnField::ReasoningEffort),
            ),
            message: turn.message.clone(),
            reasoning_content: field_from_option(
                turn.reasoning_content.clone(),
                turn_null(StorylineTurnField::ReasoningContent),
            ),
            tool_calls: field_from_option(tool_calls, turn_null(StorylineTurnField::ToolCalls)),
            observation: field_from_option(observation, turn_null(StorylineTurnField::Observation)),
            metrics,
            extra: field_from_option(turn.extra.clone(), turn_null(StorylineTurnField::Extra)),
            llm_call_count: field_from_option(
                turn.llm_call_count,
                turn_null(StorylineTurnField::LlmCallCount),
            ),
            is_copied_context: field_from_option(
                turn.is_copied_context,
                turn_null(StorylineTurnField::IsCopiedContext),
            ),
        });
    }

    let root_null = |field| story.presence.root_nulls.contains(&field);
    let agent_null = |field| story.presence.agent_nulls.contains(&field);
    let subagent_trajectories = match embedded_children {
        Some(children) => FieldPresence::Value(children),
        None if root_null(StorylineRootField::SubagentTrajectories) => FieldPresence::Null,
        None if story.child_session_ids.as_ref().is_some_and(Vec::is_empty) => {
            FieldPresence::Value(Vec::new())
        }
        None => FieldPresence::Missing,
    };

    Ok(AtifTrajectory {
        schema_version: story
            .schema_version
            .clone()
            .unwrap_or_else(|| "ATIF-v1.7".into()),
        session_id: match story.presence.session_id {
            PresenceState::Missing => FieldPresence::Missing,
            PresenceState::Null => FieldPresence::Null,
            PresenceState::Value => FieldPresence::Value(story.session_id.clone()),
        },
        trajectory_id: field_from_option(
            story.trajectory_id.clone(),
            root_null(StorylineRootField::TrajectoryId),
        ),
        agent: AtifAgent {
            name: story
                .agent
                .name
                .clone()
                .unwrap_or_else(|| story.agent.id.clone()),
            version: story.agent.version.clone().unwrap_or_default(),
            model_name: field_from_option(
                story.agent.model_name.clone(),
                agent_null(StorylineAgentField::ModelName),
            ),
            tool_definitions: field_from_option(
                story.agent.tool_definitions.clone(),
                agent_null(StorylineAgentField::ToolDefinitions),
            ),
            extra: field_from_option(
                story.agent.extra.clone(),
                agent_null(StorylineAgentField::Extra),
            ),
        },
        steps,
        notes: field_from_option(story.notes.clone(), root_null(StorylineRootField::Notes)),
        final_metrics: field_from_option(
            story.final_metrics.clone(),
            root_null(StorylineRootField::FinalMetrics),
        ),
        continued_trajectory_ref: field_from_option(
            story.continued_trajectory_ref.clone(),
            root_null(StorylineRootField::ContinuedTrajectoryRef),
        ),
        extra: field_from_option(story.extra.clone(), root_null(StorylineRootField::Extra)),
        subagent_trajectories,
    })
}

/// Rebuild one or more ATIF trees from flattened Storyline documents.
pub fn storylines_to_atif(stories: &[StorylineDocument]) -> Result<Vec<AtifTrajectory>> {
    use std::collections::{HashMap, HashSet};

    fn key(story: &StorylineDocument) -> &str {
        story
            .trajectory_id
            .as_deref()
            .unwrap_or(story.session_id.as_str())
    }

    fn build(
        key: &str,
        stories: &[StorylineDocument],
        indexes: &HashMap<String, usize>,
        visiting: &mut HashSet<String>,
        emitted: &mut HashSet<String>,
    ) -> Result<AtifTrajectory> {
        if !visiting.insert(key.to_string()) {
            return Err(Error::Other(format!(
                "Storyline child graph contains a cycle at '{key}'"
            )));
        }
        let index = indexes.get(key).ok_or_else(|| {
            Error::Other(format!("Storyline child '{key}' has no matching document"))
        })?;
        let story = &stories[*index];
        let children = match &story.child_session_ids {
            Some(child_keys) => {
                let mut children = Vec::with_capacity(child_keys.len());
                for child_key in child_keys {
                    children.push(build(child_key, stories, indexes, visiting, emitted)?);
                }
                Some(children)
            }
            None => None,
        };
        visiting.remove(key);
        emitted.insert(key.to_string());
        storyline_to_atif_node(story, children)
    }

    let mut indexes = HashMap::new();
    let mut referenced = HashSet::new();
    for (index, story) in stories.iter().enumerate() {
        let identity = key(story).to_string();
        if indexes.insert(identity.clone(), index).is_some() {
            return Err(Error::Other(format!(
                "duplicate Storyline document identity '{identity}'"
            )));
        }
        if let Some(children) = &story.child_session_ids {
            for child in children {
                if !referenced.insert(child.clone()) {
                    return Err(Error::Other(format!(
                        "Storyline child '{child}' has multiple parents"
                    )));
                }
            }
        }
    }

    let roots = stories
        .iter()
        .map(key)
        .filter(|identity| !referenced.contains(*identity))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !stories.is_empty() && roots.is_empty() {
        return Err(Error::Other(
            "Storyline child graph has no root document".into(),
        ));
    }

    let mut visiting = HashSet::new();
    let mut emitted = HashSet::new();
    let mut output = Vec::with_capacity(roots.len());
    for root in roots {
        output.push(build(
            &root,
            stories,
            &indexes,
            &mut visiting,
            &mut emitted,
        )?);
    }
    if emitted.len() != stories.len() {
        return Err(Error::Other(
            "Storyline child graph contains unreachable documents".into(),
        ));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{atif_to_storyline, atif_to_storylines, storyline_to_atif, storylines_to_atif};
    use crate::atif::AtifTrajectory;
    use crate::{DocumentFormat, Error, FieldPresence};

    #[test]
    fn malformed_atif_observation_is_not_silently_dropped() {
        let trajectory = AtifTrajectory::from_json_str(
            r#"{
                "schema_version":"ATIF-v1.7",
                "session_id":"session-1",
                "agent":{"name":"agent-1","version":"1"},
                "steps":[{"step_id":1,"source":"agent","message":"done"}]
            }"#,
        )
        .unwrap();
        let mut story = atif_to_storyline(&trajectory).unwrap();
        story.turns[0].observation = Some(serde_json::json!({"results":"not-an-array"}));

        let error = storyline_to_atif(&story).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidDocument {
                format: DocumentFormat::Atif,
                location: Some(ref location),
                ..
            } if location == "step[0].observation"
        ));
    }

    #[test]
    fn atif_tool_result_presence_round_trips_without_provenance() {
        let trajectory = AtifTrajectory::from_json_str(
            r#"{
                "schema_version":"ATIF-v1.7",
                "session_id":"session-1",
                "agent":{"name":"agent-1","version":"1"},
                "steps":[{
                    "step_id":1,
                    "source":"agent",
                    "message":"done",
                    "tool_calls":[
                        {"tool_call_id":"missing","function_name":"a","arguments":{}},
                        {"tool_call_id":"null","function_name":"b","arguments":{},"result":null},
                        {"tool_call_id":"value","function_name":"c","arguments":{},"result":{"ok":true}}
                    ]
                }]
            }"#,
        )
        .unwrap();

        let story = atif_to_storyline(&trajectory).unwrap();
        assert_eq!(story.schema_version.as_deref(), Some("ATIF-v1.7"));
        let calls = story.turns[0].tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].result, FieldPresence::Missing);
        assert_eq!(calls[1].result, FieldPresence::Null);
        assert_eq!(
            calls[2].result,
            FieldPresence::Value(serde_json::json!({"ok": true}))
        );
        assert!(calls.iter().all(|call| {
            !call
                .extra
                .as_ref()
                .is_some_and(|extra| extra.to_string().contains(&["_pchron", "icle_"].concat()))
        }));

        let encoded = serde_json::to_value(storyline_to_atif(&story).unwrap()).unwrap();
        let calls = encoded["steps"][0]["tool_calls"].as_array().unwrap();
        assert!(calls[0].get("result").is_none());
        assert_eq!(calls[1]["result"], serde_json::Value::Null);
        assert_eq!(calls[2]["result"], serde_json::json!({"ok": true}));
    }

    #[test]
    fn atif_null_presence_and_trajectory_only_identity_round_trip() {
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "trajectory_id": "trajectory-only",
            "agent": {
                "name": "agent-1",
                "version": "1",
                "model_name": null,
                "extra": null
            },
            "steps": [{
                "step_id": 1,
                "timestamp": null,
                "source": "agent",
                "message": "done",
                "reasoning_content": null,
                "tool_calls": null,
                "observation": null,
                "metrics": null,
                "extra": null,
                "llm_call_count": null,
                "is_copied_context": null
            }],
            "notes": null,
            "final_metrics": null,
            "continued_trajectory_ref": null,
            "extra": null,
            "subagent_trajectories": null
        });
        let trajectory = AtifTrajectory::from_json_str(&input.to_string()).unwrap();
        let story = atif_to_storyline(&trajectory).unwrap();
        let output = serde_json::to_value(storyline_to_atif(&story).unwrap()).unwrap();
        assert_eq!(output, input);
    }

    #[test]
    fn embedded_atif_subagents_flatten_and_rebuild_in_order() {
        let input = serde_json::json!({
            "schema_version": "ATIF-v1.7",
            "session_id": "shared-run",
            "trajectory_id": "root",
            "agent": {"name": "root-agent", "version": "1"},
            "steps": [],
            "subagent_trajectories": [
                {
                    "schema_version": "ATIF-v1.7",
                    "trajectory_id": "child-a",
                    "agent": {"name": "child-a", "version": "1"},
                    "steps": []
                },
                {
                    "schema_version": "ATIF-v1.7",
                    "session_id": "shared-run",
                    "trajectory_id": "child-b",
                    "agent": {"name": "child-b", "version": "1"},
                    "steps": []
                }
            ]
        });
        let trajectory = AtifTrajectory::from_json_str(&input.to_string()).unwrap();
        assert!(atif_to_storyline(&trajectory).is_err());
        let stories = atif_to_storylines(&trajectory).unwrap();
        assert_eq!(stories.len(), 3);
        assert_eq!(
            stories[0].child_session_ids.as_deref(),
            Some(&["child-a".into(), "child-b".into()][..])
        );
        let rebuilt = storylines_to_atif(&stories).unwrap();
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(serde_json::to_value(&rebuilt[0]).unwrap(), input);
    }

    #[test]
    fn storyline_child_graph_rejects_multiple_parents() {
        let story = |trajectory_id: &str| {
            let input = serde_json::json!({
                "schema_version": "ATIF-v1.7",
                "session_id": "shared-run",
                "trajectory_id": trajectory_id,
                "agent": {"name": trajectory_id, "version": "1"},
                "steps": []
            });
            let trajectory = AtifTrajectory::from_json_str(&input.to_string()).unwrap();
            atif_to_storyline(&trajectory).unwrap()
        };
        let mut first = story("first");
        let mut second = story("second");
        let child = story("child");
        first.child_session_ids = Some(vec!["child".into()]);
        second.child_session_ids = Some(vec!["child".into()]);

        let error = storylines_to_atif(&[first, second, child]).unwrap_err();
        assert!(error
            .to_string()
            .contains("Storyline child 'child' has multiple parents"));
    }
}
