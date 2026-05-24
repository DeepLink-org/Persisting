"""Python wrappers for Persisting trajectory core APIs (PyO3 → Rust engine)."""

from __future__ import annotations

import json
import uuid
from typing import Any, Iterable, Iterator

from persisting import _core

__all__ = [
    "append_engine_lines",
    "append_records",
    "iter_replay_pages",
    "parse_replay_record",
    "replay",
    "stats",
]


def append_engine_lines(
    storage: str,
    engine_lines: str,
    *,
    agent_id: str | None = None,
    session_id: str | None = None,
) -> dict[str, Any]:
    """Append a newline-separated engine record batch (RON lines). Prefer :func:`append_records`."""
    a = agent_id or f"a{uuid.uuid4().hex}"
    s = session_id or f"s{uuid.uuid4().hex}"
    return _core.trajectory_append(storage, a, s, engine_lines)


def append_records(
    storage: str,
    records: Iterable[dict[str, Any]],
    *,
    agent_id: str | None = None,
    session_id: str | None = None,
) -> dict[str, Any]:
    """Append capture-shaped dict records (written to Lance; markdown uses dialogue blocks when enabled)."""
    a = agent_id or f"a{uuid.uuid4().hex}"
    s = session_id or f"s{uuid.uuid4().hex}"
    return _core.trajectory_append(storage, a, s, list(records))


def replay(
    storage: str,
    *,
    agent_id: str | None = None,
    session_id: str | None = None,
    offset: int = 0,
    limit: int | None = None,
    root_session_id: str | None = None,
) -> dict[str, Any]:
    """Page through one session (``seq`` order for Lance; block order for markdown)."""
    return _core.trajectory_replay(
        storage, agent_id, session_id, offset, limit, root_session_id
    )


def stats(
    storage: str,
    *,
    agent_id: str | None = None,
    session_id: str | None = None,
    root_session_id: str | None = None,
) -> dict[str, Any]:
    """Dataset stats for a session path or ``storage/agent_id/session_id``."""
    return _core.trajectory_stats(storage, agent_id, session_id, root_session_id)


def parse_replay_record(line: str) -> dict[str, Any]:
    """Parse one ``trajectory replay`` record line (JSON with ``content`` + metadata)."""
    row = json.loads(line.strip())
    if not isinstance(row, dict):
        raise ValueError("replay record must be a JSON object")
    return row


def iter_replay_pages(
    storage: str,
    *,
    agent_id: str | None = None,
    session_id: str | None = None,
    page_size: int = 1000,
    offset: int = 0,
    root_session_id: str | None = None,
) -> Iterator[dict[str, Any]]:
    """Paginate ``replay``; each page's ``records`` entries are JSON objects (markdown storage)."""
    if page_size < 1:
        raise ValueError("page_size must be >= 1")
    o = offset
    while True:
        page = replay(
            storage,
            agent_id=agent_id,
            session_id=session_id,
            offset=o,
            limit=page_size,
            root_session_id=root_session_id,
        )
        if page.get("status") != "ok":
            yield page
            return
        raw = page.get("records") or []
        parsed: list[Any] = []
        for item in raw:
            if isinstance(item, str):
                parsed.append(parse_replay_record(item))
            else:
                parsed.append(item)
        page = {**page, "records": parsed}
        if not parsed:
            yield page
            return
        yield page
        o += len(parsed)
        if len(parsed) < page_size:
            return
