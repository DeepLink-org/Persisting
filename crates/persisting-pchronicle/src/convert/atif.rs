//! ATIF ⇄ storyline.

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::formats::storyline::{
    StorylineAgent, StorylineDocument, StorylineToolCall, StorylineTurn,
};
use crate::{DocumentFormat, Error, Result};

fn timing_from_metrics(metrics: &Option<serde_json::Value>) -> (Option<i64>, Option<i64>) {
    let Some(m) = metrics else {
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

pub fn atif_to_storyline(traj: &AtifTrajectory) -> Result<StorylineDocument> {
    let session_id = traj.effective_session_id()?.to_string();
    let child_ids: Vec<String> = traj
        .subagent_trajectories
        .as_ref()
        .map(|subs| {
            subs.iter()
                .filter_map(|sub| sub.trajectory_id.clone().or_else(|| sub.session_id.clone()))
                .collect()
        })
        .unwrap_or_default();

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
                .map(|o| serde_json::to_value(o).unwrap_or(serde_json::Value::Null)),
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
        run_id: traj.trajectory_id.clone(),
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
        parent: None,
        child_session_ids: if child_ids.is_empty() {
            None
        } else {
            Some(child_ids)
        },
        notes: traj.notes.clone(),
        final_metrics: traj.final_metrics.clone(),
        continued_trajectory_ref: traj.continued_trajectory_ref.clone(),
        extra: traj.extra.clone(),
        turns,
    })
}

pub fn storyline_to_atif(story: &StorylineDocument) -> Result<AtifTrajectory> {
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
        });
    }

    Ok(AtifTrajectory {
        schema_version: story
            .schema_version
            .clone()
            .unwrap_or_else(|| "ATIF-v1.7".into()),
        session_id: Some(story.session_id.clone()),
        trajectory_id: story.run_id.clone(),
        agent: AtifAgent {
            name: story
                .agent
                .name
                .clone()
                .unwrap_or_else(|| story.agent.id.clone()),
            version: story.agent.version.clone().unwrap_or_default(),
            model_name: story.agent.model_name.clone(),
            tool_definitions: story.agent.tool_definitions.clone(),
            extra: story.agent.extra.clone(),
        },
        steps,
        notes: story.notes.clone(),
        final_metrics: story.final_metrics.clone(),
        continued_trajectory_ref: story.continued_trajectory_ref.clone(),
        extra: story.extra.clone(),
        subagent_trajectories: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{atif_to_storyline, storyline_to_atif};
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
                .is_some_and(|extra| extra.to_string().contains("_pchronicle_"))
        }));

        let encoded = serde_json::to_value(storyline_to_atif(&story).unwrap()).unwrap();
        let calls = encoded["steps"][0]["tool_calls"].as_array().unwrap();
        assert!(calls[0].get("result").is_none());
        assert_eq!(calls[1]["result"], serde_json::Value::Null);
        assert_eq!(calls[2]["result"], serde_json::json!({"ok": true}));
    }
}
