"""atif_trajectory join view over sessions / steps / tool_calls."""

from __future__ import annotations

from typing import Any

from persisting.pchronicle.schema import SESSIONS, STEPS, TOOL_CALLS
from persisting.pchronicle.store import ChronicleStore

ATIF_TRAJECTORY_VIEW = "atif_trajectory"


class AtifTrajectoryView:
    def __init__(self, store: ChronicleStore) -> None:
        self.store = store

    def query(self, session_id: str | None = None) -> list[dict[str, Any]]:
        sessions = (
            [s for s in [self.store.get_session(session_id)] if s is not None]
            if session_id is not None
            else self.store.list_sessions()
        )
        rows: list[dict[str, Any]] = []
        for session in sessions:
            steps = self.store.list_steps(session.session_id)
            calls = self.store.list_tool_calls(session.session_id)
            by_step: dict[int, list] = {}
            for call in calls:
                by_step.setdefault(call.step_id, []).append(call)
            for step in steps:
                matched = by_step.get(step.step_id) or [None]
                for call in matched:
                    rows.append(_flatten(session, step, call))
        rows.sort(
            key=lambda r: (
                r["session_id"],
                r["step_id"],
                r.get("tool_call_id") or "",
            )
        )
        return rows


def _flatten(session, step, call) -> dict[str, Any]:
    row = {
        "session_id": session.session_id,
        "trajectory_id": session.trajectory_id,
        "schema_version": session.schema_version,
        "agent_name": session.agent_name,
        "agent_version": session.agent_version,
        "agent_model_name": session.agent_model_name,
        "notes": session.notes,
        "final_metrics": session.final_metrics,
        "step_id": step.step_id,
        "timestamp": step.timestamp,
        "source": step.source,
        "model_name": step.model_name,
        "message": step.message,
        "reasoning_content": step.reasoning_content,
        "observation": step.observation,
        "metrics": step.metrics,
        "llm_call_count": step.llm_call_count,
        "is_copied_context": step.is_copied_context,
        "tool_call_id": None if call is None else call.tool_call_id,
        "function_name": None if call is None else call.function_name,
        "arguments": None if call is None else call.arguments,
        "tool_call_extra": None if call is None else call.extra,
    }
    return {
        k: v
        for k, v in row.items()
        if v is not None or k in {"tool_call_id", "function_name", "arguments", "tool_call_extra"}
    }


def atif_trajectory_sql_ddl() -> str:
    return f"""CREATE VIEW IF NOT EXISTS {ATIF_TRAJECTORY_VIEW} AS
SELECT
  s.session_id,
  s.trajectory_id,
  s.schema_version,
  s.agent_name,
  s.agent_version,
  s.agent_model_name,
  s.notes,
  s.final_metrics,
  st.step_id,
  st.timestamp,
  st.source,
  st.model_name,
  st.message,
  st.reasoning_content,
  st.observation,
  st.metrics,
  st.llm_call_count,
  st.is_copied_context,
  tc.tool_call_id,
  tc.function_name,
  tc.arguments,
  tc.extra AS tool_call_extra
FROM {SESSIONS} AS s
JOIN {STEPS} AS st
  ON s.session_id = st.session_id
LEFT JOIN {TOOL_CALLS} AS tc
  ON st.session_id = tc.session_id
 AND st.step_id = tc.step_id;
"""
