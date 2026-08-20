//! ATIF ⇄ storyline.

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::format::DocumentFormat;
use crate::formats::storyline::{
    StoryLink, StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
use crate::formats::unknown_fields::{
    attach_carried_unknown_fields, canonical_source_document_id, restore_json_pointer,
    take_unknown_fields_envelope, validate_unknown_fields, write_foreign_unknown_fields_envelope,
    CarrierBinding, PointerWrite, UnknownFieldLimits,
};
use anyhow::Context as _;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::Result;

fn timing_from_metrics(metrics: &Option<serde_json::Value>) -> (Option<i64>, Option<i64>) {
    let Some(m) = metrics.as_ref() else {
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

#[cfg(test)]
pub fn atif_to_storyline(traj: &AtifTrajectory) -> Result<StorylineDocument> {
    if traj
        .subagent_trajectories
        .as_ref()
        .is_some_and(|children| !children.is_empty())
    {
        anyhow::bail!("embedded ATIF subagent trajectories require atif_to_storylines");
    }
    atif_to_storyline_node(traj, None, None)
}

/// Flatten one ATIF tree into authoritative Storyline documents.
///
/// Embedded subagents retain input order through each parent's `children`
/// list. Missing child `session_id` values inherit the parent's effective
/// storage identity.
#[cfg(test)]
pub fn atif_to_storylines(traj: &AtifTrajectory) -> Result<Vec<StorylineDocument>> {
    let source_id = canonical_source_document_id(&serde_json::to_value(traj)?)?;
    atif_to_storylines_with_source(traj, &source_id).map(|(stories, _)| stories)
}

fn atif_to_storylines_with_source(
    traj: &AtifTrajectory,
    source_document_id: &str,
) -> Result<(Vec<StorylineDocument>, Vec<CarrierBinding>)> {
    fn visit(
        trajectory: &AtifTrajectory,
        parent_key: Option<&str>,
        inherited_session_id: Option<&str>,
        source_document_id: &str,
        source_pointer: &str,
        output: &mut Vec<StorylineDocument>,
        carriers: &mut Vec<CarrierBinding>,
    ) -> Result<()> {
        let mut story = atif_to_storyline_node(trajectory, parent_key, inherited_session_id)?;
        capture_atif_unknowns(trajectory, source_document_id, source_pointer, &mut story)?;
        let parent_key = story
            .trajectory_id
            .as_deref()
            .unwrap_or(story.session_id.as_str())
            .to_string();
        let inherited_session_id = story.session_id.clone();
        let story_index = output.len();
        output.push(story);
        carriers.push(CarrierBinding {
            story_index,
            pointer: source_pointer.to_string(),
        });
        if let Some(children) = trajectory.subagent_trajectories.as_ref() {
            for (index, child) in children.iter().enumerate() {
                let child_pointer = pointer_join(
                    &pointer_join(source_pointer, "subagent_trajectories"),
                    &index.to_string(),
                );
                visit(
                    child,
                    Some(&parent_key),
                    Some(&inherited_session_id),
                    source_document_id,
                    &child_pointer,
                    output,
                    carriers,
                )?;
            }
        }
        Ok(())
    }

    let mut output = Vec::new();
    let mut carriers = Vec::new();
    visit(
        traj,
        None,
        None,
        source_document_id,
        "",
        &mut output,
        &mut carriers,
    )?;
    Ok((output, carriers))
}

pub(crate) fn atif_value_to_storylines(
    mut value: Value,
) -> crate::InputResult<Vec<StorylineDocument>> {
    let envelope = take_unknown_fields_envelope(&mut value)?;
    let source_document_id = canonical_source_document_id(&value)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    let trajectory: AtifTrajectory = serde_json::from_value(value)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    trajectory.validate()?;
    let (mut stories, carriers) = atif_to_storylines_with_source(&trajectory, &source_document_id)
        .map_err(|error| crate::InputIssue::invalid(error.to_string()))?;
    attach_carried_unknown_fields(
        envelope,
        &carriers,
        &mut stories,
        UnknownFieldLimits::default(),
    )?;
    for story in &mut stories {
        story.unknown_key_counts =
            validate_unknown_fields(&story.unknown_fields, UnknownFieldLimits::default())?;
    }
    Ok(stories)
}

pub(crate) fn atif_collection_to_storylines(value: Value) -> Result<Vec<StorylineDocument>> {
    atif_value_to_storylines(value).map_err(anyhow::Error::from)
}

fn atif_to_storyline_node(
    traj: &AtifTrajectory,
    parent_key: Option<&str>,
    inherited_session_id: Option<&str>,
) -> Result<StorylineDocument> {
    let session_id = traj
        .session_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .or(inherited_session_id)
        .or_else(|| {
            traj.trajectory_id
                .as_deref()
                .filter(|value| !value.is_empty())
        })
        .ok_or_else(|| anyhow::anyhow!("ATIF trajectory requires an effective storage identity"))?
        .to_string();
    let child_ids = traj.subagent_trajectories.as_ref().map(|children| {
        children
            .iter()
            .map(|child| {
                child
                    .trajectory_id
                    .as_ref()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("embedded ATIF subagent requires trajectory_id"))
                    .cloned()
            })
            .collect::<Result<Vec<_>>>()
    });
    let child_ids = child_ids.transpose()?;

    let mut turns = Vec::new();
    for step in &traj.steps {
        let tool_calls = step.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let duration_ms = c
                        .extra
                        .as_ref()
                        .and_then(|x| x.get("duration_ms"))
                        .and_then(|v| v.as_i64());
                    StorylineToolCall {
                        tool_call_id: c.tool_call_id.clone(),
                        function_name: c.function_name.clone(),
                        arguments: c.arguments.clone(),
                        result: c.result.clone(),
                        duration_ms,
                        extra: c.extra.clone(),
                    }
                })
                .collect::<Vec<_>>()
        });

        let (latency_ms, ttft_ms) = timing_from_metrics(&step.metrics);

        let mut turn = StorylineTurn {
            id: step.step_id,
            kind: None,
            timestamp: step.timestamp.clone(),
            source: step.source.clone(),
            message: step.message.clone(),
            reasoning_content: step.reasoning_content.clone(),
            reasoning_effort: step.reasoning_effort.clone(),
            tool_calls,
            observation: step
                .observation
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            metrics: step.metrics.clone(),
            model_name: step.model_name.clone(),
            llm_call_count: step.llm_call_count,
            is_copied_context: step.is_copied_context,
            latency_ms,
            ttft_ms,
            extra: step.extra.clone(),
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
        trajectory_id: traj.trajectory_id.clone(),
        attempt_id: None,
        session_id,
        agent: StorylineAgent {
            id: traj.agent.name.clone(),
            name: Some(traj.agent.name.clone()),
            version: Some(traj.agent.version.clone()),
            model_name: traj.agent.model_name.clone(),
            tool_definitions: traj.agent.tool_definitions.clone(),
            extra: traj.agent.extra.clone(),
        },
        parent: parent_key.map(|parent_session_id| StoryLink {
            parent_session_id: parent_session_id.to_string(),
            spawn_call_id: None,
            spawn_id: None,
            relation: "spawn".into(),
        }),
        child_session_ids: child_ids,
        notes: traj.notes.clone(),
        final_metrics: traj.final_metrics.clone(),
        continued_trajectory_ref: traj.continued_trajectory_ref.clone(),
        extra: traj.extra.clone(),
        unknown_fields: Default::default(),
        unknown_key_counts: Default::default(),
        turns,
    })
}

fn capture_atif_unknowns(
    trajectory: &AtifTrajectory,
    source_document_id: &str,
    source_pointer: &str,
    story: &mut StorylineDocument,
) -> crate::InputResult<()> {
    insert_unknown_map(
        story,
        source_document_id,
        source_pointer,
        &trajectory.unknown,
    )?;
    insert_unknown_map(
        story,
        source_document_id,
        &pointer_join(source_pointer, "agent"),
        &trajectory.agent.unknown,
    )?;
    for (step_index, step) in trajectory.steps.iter().enumerate() {
        let step_pointer = pointer_join(
            &pointer_join(source_pointer, "steps"),
            &step_index.to_string(),
        );
        insert_unknown_map(story, source_document_id, &step_pointer, &step.unknown)?;
        if let Some(calls) = step.tool_calls.as_ref() {
            for (call_index, call) in calls.iter().enumerate() {
                let call_pointer = pointer_join(
                    &pointer_join(&step_pointer, "tool_calls"),
                    &call_index.to_string(),
                );
                insert_unknown_map(story, source_document_id, &call_pointer, &call.unknown)?;
            }
        }
    }
    Ok(())
}

fn insert_unknown_map(
    story: &mut StorylineDocument,
    source_document_id: &str,
    parent: &str,
    fields: &Map<String, Value>,
) -> crate::InputResult<()> {
    for (key, value) in fields {
        story.unknown_fields.insert(
            "atif",
            source_document_id,
            pointer_join(parent, key),
            value.clone(),
        )?;
    }
    Ok(())
}

fn pointer_join(parent: &str, token: &str) -> String {
    format!("{parent}/{}", token.replace('~', "~0").replace('/', "~1"))
}

#[cfg(test)]
pub fn storyline_to_atif(story: &StorylineDocument) -> Result<AtifTrajectory> {
    if story
        .child_session_ids
        .as_ref()
        .is_some_and(|children| !children.is_empty())
    {
        anyhow::bail!("Storyline with child documents requires storylines_to_atif");
    }
    storyline_to_atif_node(story, None)
}

fn storyline_to_atif_node(
    story: &StorylineDocument,
    embedded_children: Option<Vec<AtifTrajectory>>,
) -> Result<AtifTrajectory> {
    if story.session_id.is_empty() || story.agent.id.is_empty() {
        anyhow::bail!("invalid Storyline identity for ATIF conversion");
    }
    let mut steps = Vec::new();
    for (step_index, turn) in story.turns.iter().enumerate() {
        let observation = turn
            .observation
            .as_ref()
            .map(|value| {
                serde_json::from_value::<AtifObservation>(value.clone()).with_context(|| {
                    format!("decode ATIF step[{step_index}].observation from Storyline")
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
                    let extra = if extra.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                        None
                    } else {
                        Some(extra)
                    };
                    AtifToolCall {
                        tool_call_id: c.tool_call_id.clone(),
                        function_name: c.function_name.clone(),
                        arguments: c.arguments.clone(),
                        result: c.result.clone(),
                        extra,
                        unknown: Map::new(),
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
        let metrics = if metrics.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            None
        } else {
            Some(metrics)
        };

        steps.push(AtifStep {
            step_id: turn.id,
            timestamp: turn.timestamp.clone(),
            source: turn.source.clone(),
            model_name: turn.model_name.clone(),
            reasoning_effort: turn.reasoning_effort.clone(),
            message: turn.message.clone(),
            reasoning_content: turn.reasoning_content.clone(),
            tool_calls,
            observation,
            metrics,
            extra: turn.extra.clone(),
            llm_call_count: turn.llm_call_count,
            is_copied_context: turn.is_copied_context,
            unknown: Map::new(),
        });
    }

    let subagent_trajectories = match embedded_children {
        Some(children) => Some(children),
        None if story.child_session_ids.as_ref().is_some_and(Vec::is_empty) => Some(Vec::new()),
        None => None,
    };

    Ok(AtifTrajectory {
        schema_version: story
            .schema_version
            .clone()
            .unwrap_or_else(|| "ATIF-v1.7".into()),
        session_id: Some(story.session_id.clone()),
        trajectory_id: story.trajectory_id.clone(),
        agent: AtifAgent {
            name: story
                .agent
                .name
                .clone()
                .unwrap_or_else(|| story.agent.id.clone()),
            version: story
                .agent
                .version
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            model_name: story.agent.model_name.clone(),
            tool_definitions: story.agent.tool_definitions.clone(),
            extra: story.agent.extra.clone(),
            unknown: Map::new(),
        },
        steps,
        notes: story.notes.clone(),
        final_metrics: story.final_metrics.clone(),
        continued_trajectory_ref: story.continued_trajectory_ref.clone(),
        extra: story.extra.clone(),
        subagent_trajectories,
        unknown: Map::new(),
    })
}

/// Rebuild one or more ATIF trees from flattened Storyline documents.
pub fn storylines_to_atif(stories: &[StorylineDocument]) -> Result<Vec<AtifTrajectory>> {
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
            anyhow::bail!("Storyline child graph contains a cycle at '{key}'");
        }
        let index = indexes
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("Storyline child '{key}' has no matching document"))?;
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
            anyhow::bail!("duplicate Storyline document identity '{identity}'");
        }
        if let Some(children) = &story.child_session_ids {
            for child in children {
                if !referenced.insert(child.clone()) {
                    anyhow::bail!("Storyline child '{child}' has multiple parents");
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
        anyhow::bail!("Storyline child graph has no root document");
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
        anyhow::bail!("Storyline child graph contains unreachable documents");
    }
    restore_atif_documents(output, stories)
}

fn restore_atif_documents(
    documents: Vec<AtifTrajectory>,
    stories: &[StorylineDocument],
) -> Result<Vec<AtifTrajectory>> {
    let single = documents.len() == 1;
    let mut value = if documents.len() == 1 {
        serde_json::to_value(&documents[0])?
    } else {
        serde_json::to_value(&documents)?
    };

    let indexes = stories
        .iter()
        .enumerate()
        .map(|(index, story)| (story.document_id().to_string(), index))
        .collect::<HashMap<_, _>>();
    let referenced = stories
        .iter()
        .flat_map(|story| story.child_session_ids.iter().flatten().cloned())
        .collect::<HashSet<_>>();
    let root_indexes = stories
        .iter()
        .enumerate()
        .filter_map(|(index, story)| (!referenced.contains(story.document_id())).then_some(index))
        .collect::<Vec<_>>();

    fn bind_tree(
        story_index: usize,
        pointer: &str,
        stories: &[StorylineDocument],
        indexes: &HashMap<String, usize>,
        carriers: &mut Vec<CarrierBinding>,
    ) -> Result<()> {
        carriers.push(CarrierBinding {
            story_index,
            pointer: pointer.to_string(),
        });
        if let Some(children) = &stories[story_index].child_session_ids {
            for (child_position, child) in children.iter().enumerate() {
                let child_index = *indexes.get(child).ok_or_else(|| {
                    anyhow::anyhow!("Storyline child '{child}' has no matching document")
                })?;
                let child_pointer = pointer_join(
                    &pointer_join(pointer, "subagent_trajectories"),
                    &child_position.to_string(),
                );
                bind_tree(child_index, &child_pointer, stories, indexes, carriers)?;
            }
        }
        Ok(())
    }

    let mut carriers = Vec::new();
    for (root_position, story_index) in root_indexes.iter().copied().enumerate() {
        let pointer = if root_indexes.len() == 1 {
            String::new()
        } else {
            pointer_join("", &root_position.to_string())
        };
        bind_tree(story_index, &pointer, stories, &indexes, &mut carriers)?;
    }

    let mut source_roots = BTreeMap::<String, usize>::new();
    for story_index in &root_indexes {
        if let Some(source) = stories[*story_index].unknown_fields.sources.get("atif") {
            if source_roots
                .insert(source.source_document_id.clone(), *story_index)
                .is_some()
            {
                anyhow::bail!(
                    "ATIF source document '{}' has multiple root trajectories",
                    source.source_document_id
                );
            }
        }
    }
    let carrier_by_story = carriers
        .iter()
        .map(|carrier| (carrier.story_index, carrier.pointer.clone()))
        .collect::<HashMap<_, _>>();
    let mut merged = BTreeMap::<String, BTreeMap<String, Value>>::new();
    for story in stories {
        let Some(source) = story.unknown_fields.sources.get("atif") else {
            continue;
        };
        let fields = merged.entry(source.source_document_id.clone()).or_default();
        for (pointer, field_value) in &source.fields {
            match fields.get(pointer) {
                Some(existing) if existing != field_value => anyhow::bail!(
                    "ATIF source '{}' has conflicting unknown field at '{}'",
                    source.source_document_id,
                    pointer
                ),
                Some(_) => {}
                None => {
                    fields.insert(pointer.clone(), field_value.clone());
                }
            }
        }
    }
    for (source_id, fields) in merged {
        let root_story = source_roots.get(&source_id).ok_or_else(|| {
            anyhow::anyhow!("ATIF source document '{source_id}' has no root trajectory")
        })?;
        let root_pointer = &carrier_by_story[root_story];
        let target = value
            .pointer_mut(root_pointer)
            .ok_or_else(|| anyhow::anyhow!("ATIF root carrier '{root_pointer}' is missing"))?;
        for (pointer, field_value) in fields {
            restore_json_pointer(target, &pointer, field_value, PointerWrite::InsertOnly)
                .with_context(|| {
                    format!(
                        "restore ATIF unknown-field pointer '{pointer}' for trajectory '{}'",
                        stories[*root_story].document_id()
                    )
                })?;
        }
    }

    write_foreign_unknown_fields_envelope(DocumentFormat::Atif, &mut value, stories, &carriers)?;
    if single {
        Ok(vec![serde_json::from_value(value)?])
    } else {
        Ok(serde_json::from_value(value)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{atif_to_storyline, atif_to_storylines, storyline_to_atif, storylines_to_atif};
    use crate::atif::AtifTrajectory;
    use crate::StorylineDocument;

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
        assert!(
            error
                .to_string()
                .contains("decode ATIF step[0].observation from Storyline"),
            "unexpected error: {error:#}"
        );
        assert!(
            error.chain().count() >= 2,
            "missing source chain: {error:#}"
        );
    }

    #[test]
    fn atif_tool_result_null_and_missing_canonicalize_to_absent() {
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
        assert_eq!(calls[0].result, None);
        assert_eq!(calls[1].result, None);
        assert_eq!(calls[2].result, Some(serde_json::json!({"ok": true})));
        assert!(calls.iter().all(|call| {
            !call
                .extra
                .as_ref()
                .is_some_and(|extra| extra.to_string().contains(&["_pchron", "icle_"].concat()))
        }));

        let encoded = serde_json::to_value(storyline_to_atif(&story).unwrap()).unwrap();
        let calls = encoded["steps"][0]["tool_calls"].as_array().unwrap();
        assert!(calls[0].get("result").is_none());
        assert!(calls[1].get("result").is_none());
        assert_eq!(calls[2]["result"], serde_json::json!({"ok": true}));
    }

    #[test]
    fn atif_null_fields_canonicalize_to_absent() {
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
        assert_eq!(output["session_id"], "trajectory-only");
        assert_eq!(output["trajectory_id"], "trajectory-only");
        assert!(output.get("notes").is_none());
        assert!(output["steps"][0].get("timestamp").is_none());
    }

    #[test]
    fn cross_format_storyline_gets_a_valid_atif_agent_version() {
        let story = StorylineDocument::new("session", "agent");
        let trajectory = storyline_to_atif(&story).unwrap();
        assert_eq!(trajectory.agent.version, "unknown");
        trajectory.validate().unwrap();
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
        let rebuilt = serde_json::to_value(&rebuilt[0]).unwrap();
        assert_eq!(
            rebuilt["subagent_trajectories"][0]["session_id"],
            "shared-run"
        );
        assert_eq!(
            rebuilt["subagent_trajectories"][0]["trajectory_id"],
            "child-a"
        );
        assert_eq!(
            rebuilt["subagent_trajectories"][1]["trajectory_id"],
            "child-b"
        );
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
