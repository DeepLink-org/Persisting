"""atif_trajectory join view over sessions / steps / tool_calls."""

from __future__ import annotations

from typing import Any

from persisting import _core
from persisting.pchronicle.store import ChronicleStore

ATIF_TRAJECTORY_VIEW = "atif_trajectory"


class AtifTrajectoryView:
    def __init__(self, store: ChronicleStore) -> None:
        self.store = store

    def query(self, session_id: str | None = None) -> list[dict[str, Any]]:
        return self.store._inner.query(session_id)


def atif_trajectory_sql_ddl() -> str:
    return _core.pchronicle_atif_trajectory_sql_ddl()
