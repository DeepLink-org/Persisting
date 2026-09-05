# Recorded data, views, and versions

pChronicle separates what happened from the views used to inspect it.

| Layer | Role | Examples |
| --- | --- | --- |
| Recorded facts | durable write-time record | lifecycle, model, tool, output, and terminal events |
| Query view | normalized query model | `runs`, `steps`, `tool_calls`, `trajectories` |
| Human-readable view | readable diagnostic output | AgenticMD |
| Exchange format | interoperability boundary | ATIF, ACTF, OpenAI Messages, Storyline JSON |
| Derived version | transformed data with a recorded origin | cleaned, redacted, or augmented Runs |

Canonical events are append-oriented facts. A projection may reorganize those
facts for a session or query, but it must not silently become a second source
of truth. Rebuildable views record their input Snapshot and transform version.

Storyline is a session-oriented projection. Its three-table Lance layout is
optimized for reconstructing a complete document; it is not a time-series
database and does not replace the canonical event path.

AgenticMD is a non-authoritative human-readable projection. A missing or stale
Markdown view does not change the canonical event result.

For Gateway-backed point observation, the Warehouse may reopen the latest
canonical event manifest for an already resolved source. This keeps active
traces current without requiring the materialized Storyline projection to be
published on every append.

A derived version, called a Revision in the storage API, points to its parent
and the transform that produced it. Cleaning, redaction, and augmentation therefore create a new history branch rather than
rewriting history without a trace.

Read [Run storage design](../design/trajectory-storage.md) for ownership and
[Run data formats](../reference/formats/index.md) for exact interchange
contracts.
