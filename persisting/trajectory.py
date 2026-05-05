"""Python wrappers for Persisting trajectory core APIs (PyO3 → Rust engine)."""

from __future__ import annotations

from typing import Any, Iterable

from persisting import _core


def append_ronl(storage: str, name: str, records_ronl: str) -> dict[str, Any]:
    """Append a newline-separated RON body (one record object per non-empty line)."""
    return _core.trajectory_append(storage, name, records_ronl)


def append_records(storage: str, name: str, records: Iterable[dict[str, Any]]) -> dict[str, Any]:
    """Append iterable of dict-like records (serialized to RON per line in Rust)."""
    return _core.trajectory_append(storage, name, list(records))


def replay(
    storage: str,
    name: str,
    *,
    trajectory_id: str | None = None,
    offset: int = 0,
    limit: int | None = None,
) -> dict[str, Any]:
    return _core.trajectory_replay(storage, name, trajectory_id, offset, limit)


def stats(storage: str, name: str) -> dict[str, Any]:
    return _core.trajectory_stats(storage, name)


__all__ = ["append_records", "append_ronl", "replay", "stats"]
