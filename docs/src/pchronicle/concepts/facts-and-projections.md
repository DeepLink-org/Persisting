# Facts, projections, and revisions

pChronicle separates what happened from the views used to inspect it.

| Layer | Role | Examples |
| --- | --- | --- |
| Canonical facts | durable write-time record | lifecycle, model, tool, artifact, and terminal events |
| Logical projection | normalized query model | `runs`, `steps`, `tool_calls`, `trajectories` |
| Human projection | readable diagnostic view | AgenticMD |
| Exchange representation | interoperability boundary | ATIF, ACTF, OpenAI Messages, Storyline JSON |
| Revision | derived data with lineage | cleaned or augmented trajectories, judgments |

Canonical events are append-oriented facts. A projection may reorganize those
facts for a session or query, but it must not silently become a second source
of truth. Rebuildable views record their input Snapshot and transform version.

Storyline is a session-oriented projection. Its three-table Lance layout is
optimized for reconstructing a complete document; it is not a time-series
database and does not replace the canonical event path.

AgenticMD is a non-authoritative human-readable projection. A missing or stale
Markdown view does not change the canonical event result.

A revision points to its parent and the transform that produced it. Cleaning,
redaction, augmentation, and judgment therefore create new lineage rather than
rewriting history without a trace.

Read [Trajectory storage](../design/trajectory-storage.md) for ownership and
[Trajectory formats](../reference/formats/index.md) for exact interchange
contracts.
