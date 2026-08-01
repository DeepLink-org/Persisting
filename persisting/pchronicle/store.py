"""Python adapters for Rust-owned pChronicle stores."""

from __future__ import annotations

from dataclasses import asdict
from pathlib import Path
from typing import Protocol

from persisting import _core
from persisting.pchronicle.schema import (
    SessionRow,
    StepRow,
    ToolCallRow,
)


class ChronicleStore(Protocol):
    def upsert_session(self, row: SessionRow) -> None: ...
    def get_session(self, session_id: str) -> SessionRow | None: ...
    def list_sessions(self) -> list[SessionRow]: ...
    def replace_steps(self, session_id: str, rows: list[StepRow]) -> None: ...
    def list_steps(self, session_id: str) -> list[StepRow]: ...
    def replace_tool_calls(self, session_id: str, rows: list[ToolCallRow]) -> None: ...
    def list_tool_calls(self, session_id: str) -> list[ToolCallRow]: ...


class MemoryChronicleStore:
    def __init__(self) -> None:
        self._inner = _core._PChronicleStore()

    def upsert_session(self, row: SessionRow) -> None:
        self._inner.upsert_session(_row_dict(row))

    def get_session(self, session_id: str) -> SessionRow | None:
        row = self._inner.get_session(session_id)
        return None if row is None else SessionRow(**row)

    def list_sessions(self) -> list[SessionRow]:
        return [SessionRow(**row) for row in self._inner.list_sessions()]

    def replace_steps(self, session_id: str, rows: list[StepRow]) -> None:
        self._inner.replace_steps(session_id, [_row_dict(row) for row in rows])

    def list_steps(self, session_id: str) -> list[StepRow]:
        return [StepRow(**row) for row in self._inner.list_steps(session_id)]

    def replace_tool_calls(self, session_id: str, rows: list[ToolCallRow]) -> None:
        self._inner.replace_tool_calls(session_id, [_row_dict(row) for row in rows])

    def list_tool_calls(self, session_id: str) -> list[ToolCallRow]:
        return [ToolCallRow(**row) for row in self._inner.list_tool_calls(session_id)]


class FsChronicleStore(MemoryChronicleStore):
    """Rust pChronicle filesystem store rooted at ``root``."""

    def __init__(self, root: str | Path) -> None:
        self.root = Path(root)
        self._inner = _core._PChronicleStore(str(self.root))


def _row_dict(row: SessionRow | StepRow | ToolCallRow) -> dict:
    return {key: value for key, value in asdict(row).items() if value is not None}
