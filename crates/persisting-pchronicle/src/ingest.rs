//! Split / ingest ATIF documents into the three normalized tables.

use crate::atif::{AtifAgent, AtifObservation, AtifStep, AtifToolCall, AtifTrajectory};
use crate::schema::{SessionRow, StepRow, ToolCallRow};
use crate::store::ChronicleStore;
use crate::Result;

/// Result of normalizing one ATIF trajectory into three tables.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitTables {
    pub session: SessionRow,
    pub steps: Vec<StepRow>,
    pub tool_calls: Vec<ToolCallRow>,
}

/// Split an ATIF trajectory into session / steps / tool_calls rows.
pub fn split_trajectory(traj: &AtifTrajectory) -> Result<SplitTables> {
    traj.validate()?;
    let session_id = traj.effective_session_id()?.to_string();

    let session = SessionRow {
        session_id: session_id.clone(),
        trajectory_id: traj.trajectory_id.clone(),
        schema_version: traj.schema_version.clone(),
        agent_name: traj.agent.name.clone(),
        agent_version: traj.agent.version.clone(),
        agent_model_name: traj.agent.model_name.clone(),
        agent_tool_definitions: traj.agent.tool_definitions.clone(),
        agent_extra: traj.agent.extra.clone(),
        notes: traj.notes.clone(),
        final_metrics: traj.final_metrics.clone(),
        continued_trajectory_ref: traj.continued_trajectory_ref.clone(),
        extra: traj.extra.clone(),
        subagent_trajectories: traj
            .subagent_trajectories
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
    };

    let mut steps = Vec::with_capacity(traj.steps.len());
    let mut tool_calls = Vec::new();
    for step in &traj.steps {
        steps.push(StepRow {
            session_id: session_id.clone(),
            step_id: step.step_id,
            timestamp: step.timestamp.clone(),
            source: step.source.clone(),
            model_name: step.model_name.clone(),
            reasoning_effort: step.reasoning_effort.clone(),
            message: step.message.clone(),
            reasoning_content: step.reasoning_content.clone(),
            observation: step
                .observation
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            metrics: step.metrics.clone(),
            extra: step.extra.clone(),
            llm_call_count: step.llm_call_count,
            is_copied_context: step.is_copied_context,
        });
        if let Some(calls) = &step.tool_calls {
            for call in calls {
                tool_calls.push(ToolCallRow {
                    session_id: session_id.clone(),
                    step_id: step.step_id,
                    tool_call_id: call.tool_call_id.clone(),
                    function_name: call.function_name.clone(),
                    arguments: call.arguments.clone(),
                    extra: call.extra.clone(),
                });
            }
        }
    }

    Ok(SplitTables {
        session,
        steps,
        tool_calls,
    })
}

/// Persist a split ATIF trajectory into a [`ChronicleStore`].
pub fn ingest_trajectory(store: &mut dyn ChronicleStore, traj: &AtifTrajectory) -> Result<String> {
    let split = split_trajectory(traj)?;
    let session_id = split.session.session_id.clone();
    store.upsert_session(split.session)?;
    store.replace_steps(&session_id, split.steps)?;
    store.replace_tool_calls(&session_id, split.tool_calls)?;
    Ok(session_id)
}

/// Rebuild an ATIF trajectory document from the three tables.
pub fn reconstruct_trajectory(
    store: &dyn ChronicleStore,
    session_id: &str,
) -> Result<AtifTrajectory> {
    let session = store
        .get_session(session_id)?
        .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;
    let steps = store.list_steps(session_id)?;
    let tool_calls = store.list_tool_calls(session_id)?;

    let mut calls_by_step: std::collections::BTreeMap<i64, Vec<AtifToolCall>> =
        std::collections::BTreeMap::new();
    for call in tool_calls {
        calls_by_step
            .entry(call.step_id)
            .or_default()
            .push(AtifToolCall {
                tool_call_id: call.tool_call_id,
                function_name: call.function_name,
                arguments: call.arguments,
                extra: call.extra,
            });
    }

    let atif_steps = steps
        .into_iter()
        .map(|step| {
            let observation = match step.observation {
                Some(v) => Some(serde_json::from_value::<AtifObservation>(v)?),
                None => None,
            };
            Ok(AtifStep {
                step_id: step.step_id,
                timestamp: step.timestamp,
                source: step.source,
                model_name: step.model_name,
                reasoning_effort: step.reasoning_effort,
                message: step.message,
                reasoning_content: step.reasoning_content,
                tool_calls: calls_by_step
                    .remove(&step.step_id)
                    .filter(|v| !v.is_empty()),
                observation,
                metrics: step.metrics,
                extra: step.extra,
                llm_call_count: step.llm_call_count,
                is_copied_context: step.is_copied_context,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let subagent_trajectories = match session.subagent_trajectories {
        Some(v) if !v.is_null() => Some(serde_json::from_value(v)?),
        _ => None,
    };

    Ok(AtifTrajectory {
        schema_version: session.schema_version,
        session_id: Some(session.session_id),
        trajectory_id: session.trajectory_id,
        agent: AtifAgent {
            name: session.agent_name,
            version: session.agent_version,
            model_name: session.agent_model_name,
            tool_definitions: session.agent_tool_definitions,
            extra: session.agent_extra,
        },
        steps: atif_steps,
        notes: session.notes,
        final_metrics: session.final_metrics,
        continued_trajectory_ref: session.continued_trajectory_ref,
        extra: session.extra,
        subagent_trajectories,
    })
}
