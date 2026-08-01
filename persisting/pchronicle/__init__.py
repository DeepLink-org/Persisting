"""Python compatibility surface for the Rust-owned pChronicle layer.

* `sessions` keyed by `session_id`
* `steps` keyed by `(session_id, step_id)`
* `tool_calls` keyed by `(session_id, tool_call_id)`
* `atif_trajectory` view = sessions ⋈ steps ⟕ tool_calls

Formats, validation, storage, reconstruction, and view semantics execute in
the Rust `persisting-pchronicle` crate. Python types are compatibility DTOs.
"""

from __future__ import annotations

from persisting.pchronicle.atif import (
    AtifTrajectory,
    ingest_trajectory,
    reconstruct_trajectory,
    split_trajectory,
)
from persisting.pchronicle.schema import SessionRow, StepRow, ToolCallRow
from persisting.pchronicle.store import FsChronicleStore, MemoryChronicleStore
from persisting.pchronicle.view import (
    ATIF_TRAJECTORY_VIEW,
    AtifTrajectoryView,
    atif_trajectory_sql_ddl,
)

__all__ = [
    "ATIF_TRAJECTORY_VIEW",
    "AtifTrajectory",
    "AtifTrajectoryView",
    "FsChronicleStore",
    "MemoryChronicleStore",
    "SessionRow",
    "StepRow",
    "ToolCallRow",
    "atif_trajectory_sql_ddl",
    "ingest_trajectory",
    "reconstruct_trajectory",
    "split_trajectory",
]
