# pChronicle architecture

pChronicle processes Agent trajectory data through one pipeline:

```text
ATIF / ACTF / OpenAI Messages / Codex / Claude Code
  → decode and validate Storyline
  → direct Storyline Lance storage
  → pinned Snapshot and DataFusion query
  → CLI / Warehouse API / Web UI / export
```

AgenticMD and Storyline JSON are additional Storyline encodings. Compact JSONL
stores arbitrary JSON records without assigning trajectory semantics.

## Ownership

| Component | Responsibility |
| --- | --- |
| `persisting-pchronicle` | models, codecs, importable sources, Lance storage, Snapshot, query |
| `persisting-pchronicle-cli` | commands, import/export orchestration, loopback Warehouse HTTP, embedded assets |
| `pchronicle-web` | browsing, analysis, storage inspection, Assistant |

A Dataset is a normalized local path or object-store URI. Pins and Warehouse
mount names resolve to paths. External entity IDs remain Source-local; rows
retain their source path. The optional Directory authenticates users and
resolves authorized paths; it does not store trajectory data.

## Reads

Discovery freezes Source membership and version references. Physical Sources
open lazily after Dataset and `_file_` pruning. One operation uses its pinned
Snapshot; independent Sources do not share a global transaction time.
Normalized tables are `sources`, `runs`, `steps`, `tool_calls`, and
`trajectories`. Compact JSONL exposes record tables.

CLI and Web share query semantics. SQL is bounded and read-only; mutation,
network functions, and arbitrary filesystem functions are rejected.

## Writes

Import decodes supported formats and writes Storyline directly. A Storyline
store publishes exact table versions through `CURRENT` after referenced
objects and table changes are durable. Failed publication leaves the previous
Snapshot readable. Writer leases and generation checks prevent concurrent
replacement from silently losing data. Maintenance compacts and collects
unreachable content using the store's publication contract.

## Warehouse

`pchronicle serve` mounts named paths on a loopback listener. Refresh builds a
replacement Snapshot before switching readers. With `--catalog-config`, the
Directory authenticates requests and dispatches data operations to bounded
workers scoped to authorized mounts and backend credentials. Without it,
local mounted paths are available through the loopback API.

See [Snapshot](catalog.md), [trajectory storage](trajectory-storage.md),
[Storyline Lance](storyline-lance.md), and the [CLI reference](../reference/cli.md).
