"""Normalized ATIF table row types."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Any

SESSIONS = "sessions"
STEPS = "steps"
TOOL_CALLS = "tool_calls"


@dataclass
class SessionRow:
    session_id: str
    schema_version: str
    agent_name: str
    agent_version: str
    trajectory_id: str | None = None
    agent_model_name: str | None = None
    agent_tool_definitions: Any | None = None
    agent_extra: Any | None = None
    notes: str | None = None
    final_metrics: Any | None = None
    continued_trajectory_ref: str | None = None
    extra: Any | None = None
    subagent_trajectories: Any | None = None

    def to_dict(self) -> dict[str, Any]:
        return _drop_none(asdict(self))


@dataclass
class StepRow:
    session_id: str
    step_id: int
    source: str
    message: Any
    timestamp: str | None = None
    model_name: str | None = None
    reasoning_effort: Any | None = None
    reasoning_content: str | None = None
    observation: Any | None = None
    metrics: Any | None = None
    extra: Any | None = None
    llm_call_count: int | None = None
    is_copied_context: bool | None = None

    def to_dict(self) -> dict[str, Any]:
        return _drop_none(asdict(self))


@dataclass
class ToolCallRow:
    session_id: str
    step_id: int
    tool_call_id: str
    function_name: str
    arguments: Any = field(default_factory=dict)
    extra: Any | None = None

    def to_dict(self) -> dict[str, Any]:
        return _drop_none(asdict(self))


def _drop_none(d: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in d.items() if v is not None}
