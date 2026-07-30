"""ATIF document helpers and table split/reconstruct."""

from __future__ import annotations

from typing import Any

from persisting.pchronicle.schema import SessionRow, StepRow, ToolCallRow
from persisting.pchronicle.store import ChronicleStore


class AtifTrajectory(dict):
    """Thin dict wrapper for ATIF JSON objects."""

    @classmethod
    def from_obj(cls, obj: dict[str, Any]) -> "AtifTrajectory":
        if not isinstance(obj, dict):
            raise TypeError("ATIF trajectory must be a dict")
        return cls(obj)

    def effective_session_id(self) -> str:
        for key in ("session_id", "trajectory_id"):
            val = self.get(key)
            if isinstance(val, str) and val:
                return val
        raise ValueError("ATIF trajectory requires session_id or trajectory_id")


def split_trajectory(traj: dict[str, Any]) -> tuple[SessionRow, list[StepRow], list[ToolCallRow]]:
    t = AtifTrajectory.from_obj(traj)
    session_id = t.effective_session_id()
    agent = t.get("agent") or {}
    session = SessionRow(
        session_id=session_id,
        trajectory_id=t.get("trajectory_id"),
        schema_version=str(t.get("schema_version") or ""),
        agent_name=str(agent.get("name") or ""),
        agent_version=str(agent.get("version") or ""),
        agent_model_name=agent.get("model_name"),
        agent_tool_definitions=agent.get("tool_definitions"),
        agent_extra=agent.get("extra"),
        notes=t.get("notes"),
        final_metrics=t.get("final_metrics"),
        continued_trajectory_ref=t.get("continued_trajectory_ref"),
        extra=t.get("extra"),
        subagent_trajectories=t.get("subagent_trajectories"),
    )
    steps: list[StepRow] = []
    tool_calls: list[ToolCallRow] = []
    for step in t.get("steps") or []:
        step_id = int(step["step_id"])
        steps.append(
            StepRow(
                session_id=session_id,
                step_id=step_id,
                timestamp=step.get("timestamp"),
                source=str(step.get("source") or ""),
                model_name=step.get("model_name"),
                reasoning_effort=step.get("reasoning_effort"),
                message=step.get("message"),
                reasoning_content=step.get("reasoning_content"),
                observation=step.get("observation"),
                metrics=step.get("metrics"),
                extra=step.get("extra"),
                llm_call_count=step.get("llm_call_count"),
                is_copied_context=step.get("is_copied_context"),
            )
        )
        for call in step.get("tool_calls") or []:
            tool_calls.append(
                ToolCallRow(
                    session_id=session_id,
                    step_id=step_id,
                    tool_call_id=str(call["tool_call_id"]),
                    function_name=str(call.get("function_name") or ""),
                    arguments=call.get("arguments") or {},
                    extra=call.get("extra"),
                )
            )
    return session, steps, tool_calls


def ingest_trajectory(store: ChronicleStore, traj: dict[str, Any]) -> str:
    session, steps, tool_calls = split_trajectory(traj)
    store.upsert_session(session)
    store.replace_steps(session.session_id, steps)
    store.replace_tool_calls(session.session_id, tool_calls)
    return session.session_id


def reconstruct_trajectory(store: ChronicleStore, session_id: str) -> dict[str, Any]:
    session = store.get_session(session_id)
    if session is None:
        raise KeyError(f"session not found: {session_id}")
    steps = store.list_steps(session_id)
    tool_calls = store.list_tool_calls(session_id)
    by_step: dict[int, list[dict[str, Any]]] = {}
    for call in tool_calls:
        by_step.setdefault(call.step_id, []).append(
            {
                "tool_call_id": call.tool_call_id,
                "function_name": call.function_name,
                "arguments": call.arguments,
                **({"extra": call.extra} if call.extra is not None else {}),
            }
        )
    atif_steps = []
    for step in steps:
        row: dict[str, Any] = {
            "step_id": step.step_id,
            "source": step.source,
            "message": step.message,
        }
        for key, val in {
            "timestamp": step.timestamp,
            "model_name": step.model_name,
            "reasoning_effort": step.reasoning_effort,
            "reasoning_content": step.reasoning_content,
            "observation": step.observation,
            "metrics": step.metrics,
            "extra": step.extra,
            "llm_call_count": step.llm_call_count,
            "is_copied_context": step.is_copied_context,
        }.items():
            if val is not None:
                row[key] = val
        calls = by_step.get(step.step_id)
        if calls:
            row["tool_calls"] = calls
        atif_steps.append(row)

    out: dict[str, Any] = {
        "schema_version": session.schema_version,
        "session_id": session.session_id,
        "agent": {
            "name": session.agent_name,
            "version": session.agent_version,
            **({"model_name": session.agent_model_name} if session.agent_model_name else {}),
            **(
                {"tool_definitions": session.agent_tool_definitions}
                if session.agent_tool_definitions is not None
                else {}
            ),
            **({"extra": session.agent_extra} if session.agent_extra is not None else {}),
        },
        "steps": atif_steps,
    }
    for key, val in {
        "trajectory_id": session.trajectory_id,
        "notes": session.notes,
        "final_metrics": session.final_metrics,
        "continued_trajectory_ref": session.continued_trajectory_ref,
        "extra": session.extra,
        "subagent_trajectories": session.subagent_trajectories,
    }.items():
        if val is not None:
            out[key] = val
    return out
