"""In-memory and JSONL filesystem stores for ATIF tables."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Protocol

from persisting.pchronicle.schema import (
    SESSIONS,
    STEPS,
    TOOL_CALLS,
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
        self._sessions: dict[str, SessionRow] = {}
        self._steps: dict[str, list[StepRow]] = {}
        self._tool_calls: dict[str, list[ToolCallRow]] = {}

    def upsert_session(self, row: SessionRow) -> None:
        self._sessions[row.session_id] = row

    def get_session(self, session_id: str) -> SessionRow | None:
        return self._sessions.get(session_id)

    def list_sessions(self) -> list[SessionRow]:
        return list(self._sessions.values())

    def replace_steps(self, session_id: str, rows: list[StepRow]) -> None:
        for row in rows:
            if row.session_id != session_id:
                raise ValueError(f"step session_id mismatch: {row.session_id} != {session_id}")
        self._steps[session_id] = sorted(rows, key=lambda r: r.step_id)

    def list_steps(self, session_id: str) -> list[StepRow]:
        return list(self._steps.get(session_id, []))

    def replace_tool_calls(self, session_id: str, rows: list[ToolCallRow]) -> None:
        step_ids = {s.step_id for s in self.list_steps(session_id)}
        for row in rows:
            if row.session_id != session_id:
                raise ValueError(f"tool_call session_id mismatch: {row.session_id} != {session_id}")
            if row.step_id not in step_ids:
                raise ValueError(
                    f"orphan tool_call {row.tool_call_id} references missing step {row.step_id}"
                )
        self._tool_calls[session_id] = sorted(rows, key=lambda r: (r.step_id, r.tool_call_id))

    def list_tool_calls(self, session_id: str) -> list[ToolCallRow]:
        return list(self._tool_calls.get(session_id, []))


class FsChronicleStore(MemoryChronicleStore):
    """JSONL tables under ``{root}/sessions|steps|tool_calls.jsonl``."""

    def __init__(self, root: str | Path) -> None:
        super().__init__()
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)
        self._reload()

    def _path(self, name: str) -> Path:
        return self.root / f"{name}.jsonl"

    def _reload(self) -> None:
        self._sessions.clear()
        self._steps.clear()
        self._tool_calls.clear()
        for obj in _read_jsonl(self._path(SESSIONS)):
            row = SessionRow(**obj)
            self._sessions[row.session_id] = row
        for obj in _read_jsonl(self._path(STEPS)):
            row = StepRow(**obj)
            self._steps.setdefault(row.session_id, []).append(row)
        for sid, rows in self._steps.items():
            self._steps[sid] = sorted(rows, key=lambda r: r.step_id)
        for obj in _read_jsonl(self._path(TOOL_CALLS)):
            row = ToolCallRow(**obj)
            self._tool_calls.setdefault(row.session_id, []).append(row)
        for sid, rows in self._tool_calls.items():
            self._tool_calls[sid] = sorted(rows, key=lambda r: (r.step_id, r.tool_call_id))

    def _persist(self) -> None:
        _write_jsonl(self._path(SESSIONS), [r.to_dict() for r in self.list_sessions()])
        steps = [r.to_dict() for rows in self._steps.values() for r in rows]
        steps.sort(key=lambda r: (r["session_id"], r["step_id"]))
        _write_jsonl(self._path(STEPS), steps)
        calls = [r.to_dict() for rows in self._tool_calls.values() for r in rows]
        calls.sort(key=lambda r: (r["session_id"], r["step_id"], r["tool_call_id"]))
        _write_jsonl(self._path(TOOL_CALLS), calls)

    def upsert_session(self, row: SessionRow) -> None:
        super().upsert_session(row)
        self._persist()

    def replace_steps(self, session_id: str, rows: list[StepRow]) -> None:
        super().replace_steps(session_id, rows)
        self._persist()

    def replace_tool_calls(self, session_id: str, rows: list[ToolCallRow]) -> None:
        super().replace_tool_calls(session_id, rows)
        self._persist()


def _read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    out: list[dict] = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            out.append(json.loads(line))
    return out


def _write_jsonl(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        for row in rows:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
