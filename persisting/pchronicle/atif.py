"""ATIF adapters backed by the canonical Rust pChronicle implementation."""

from __future__ import annotations

from typing import Any

from persisting.pchronicle.schema import SessionRow, StepRow, ToolCallRow
from persisting.pchronicle.store import ChronicleStore, MemoryChronicleStore


class AtifTrajectory(dict):
    """Thin dict wrapper for ATIF JSON objects."""

    @classmethod
    def from_obj(cls, obj: dict[str, Any]) -> "AtifTrajectory":
        if not isinstance(obj, dict):
            raise TypeError("ATIF trajectory must be a dict")
        return cls(obj)

    def effective_session_id(self) -> str:
        session, _, _ = MemoryChronicleStore()._inner.split(self)
        return session["session_id"]

    def validate(self) -> None:
        MemoryChronicleStore()._inner.split(self)


def split_trajectory(traj: dict[str, Any]) -> tuple[SessionRow, list[StepRow], list[ToolCallRow]]:
    session, steps, tool_calls = MemoryChronicleStore()._inner.split(
        AtifTrajectory.from_obj(traj)
    )
    return (
        SessionRow(**session),
        [StepRow(**row) for row in steps],
        [ToolCallRow(**row) for row in tool_calls],
    )


def ingest_trajectory(store: ChronicleStore, traj: dict[str, Any]) -> str:
    return store._inner.ingest(AtifTrajectory.from_obj(traj))


def reconstruct_trajectory(store: ChronicleStore, session_id: str) -> dict[str, Any]:
    return store._inner.reconstruct(session_id)
