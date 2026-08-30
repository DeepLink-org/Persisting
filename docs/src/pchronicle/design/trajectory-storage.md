# pChronicle run storage

> Current implementation notes. Normative ownership is in
> [RFC-0003](../../rfcs/0003-pchronicle-ownership.md) and
> [RFC-0007](../../rfcs/0007-events-contract-pchronicle-sidecar.md).
> Dataset commands are in [`pchronicle`](../reference/cli.md).

Exchange-format wire contracts and field-by-field conversions follow
[RFC-0001 § Wire schema](../../rfcs/0001-storyline-format.md#wire-schema),
[RFC-0004 § ACTF mapping](../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping),
[RFC-0008 § ATIF mapping](../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping),
and
[RFC-0009 § OpenAI Messages mapping](../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping).

## 1. Role

`persisting-pchronicle` is the structured store for Agent run data. It
owns:

- the mapping from the shared `persisting-events::EventRecord` to the Lance
  `EventRow` and its physical schema;
- Run / Story coordinates, directory layout, and discovery rules;
- read, write, stats, and maintenance for Lance canonical events;
- generation and tolerant parsing of the AgenticMD human/debug view;
- conversion among events, Storyline, ATIF, ACTF, OpenAI messages, and
  AgenticMD;
- materialize, revision lineage, and the standard query views.

`persisting-events` owns the storage-independent logical event envelope.
Gateway and pVisor produce events. The CLI can call pChronicle in-process;
pVisor can also submit through the Control service of `pchronicle serve`.
None of those producers define a second on-disk run format.

## 2. Logical coordinates

```text
Run
└── Storyline
    └── Turn
        └── Call
            └── EventRecord
```

Offline storage uses `StoryCoords`:

| Field | Meaning |
|---|---|
| `storage` | pChronicle root directory |
| `agent_id` | Agent identity; a single path segment |
| `root_session_id` | optional Run identity; shared by the main Agent and subagents |
| `session_id` | Story identity and the Lance partition key |

When `root_session_id` is present, several Stories share one Run-level
`events.lance`. Otherwise `session_id` is itself the directory boundary.

## 3. Storage layouts

### Lance events

`events.lance/` is the Run-level container for the complete event
representation. `_manifest.json` holds the active writer fence and the
visible segment versions. Each writer epoch uses its own Lance segment.
The store keeps HTTP/model calls, time, identity, payload, and order.

Gateway's durable micro-batches seal an L0 segment after every eight small
fragments. Background maintenance merges eight consecutive sealed segments
at the same level and promotes the result to the next level. The merge uses
`level` and `sealed` metadata in the manifest, replaces only an exactly
matching contiguous segment range, and never includes the active tail.
Visible segment count therefore grows with the number of levels rather than
linearly with events. Old versions and files are still vacuumed on the
maintenance retention window so readers pinned to an older Snapshot are not
broken.

The physical schema lifts `event_id` into its own business column and
normalizes `timestamp` to UTC `Timestamp(Millisecond)`. Newly written
Gateway and pVisor `EventRecord`s supply both an RFC3339 `timestamp` and
`timestamp_unix_ms`; the two values must agree at millisecond precision.
Admission still fills missing values for older producers or compatibility
imports from the RFC3339 `timestamp` or the receive time. Storyline
projections also emit UTC millisecond text from `timestamp_unix_ms`; the
original text timestamp stays in `payload_json`. The fact layer does not
check `event_id` uniqueness and does not maintain an index on it. Duplicate
IDs and retry rows are valid facts. The complete `EventRecord` remains in
`payload_json`, so replay does not drop fields. Workflows that need audit
fidelity should use the canonical events layer.

### AgenticMD

AgenticMD is a human-facing Markdown debugging view. It keeps visible
dialogue blocks and a session summary, which makes it useful for live
inspection, code review, and manual analysis. It omits protocol noise and
allows missing or extended fields, so it is not a lossless substitute for
the storage format or the raw HTTP events.

`pvisor run --record-format lance --record-destination WAREHOUSE` starts a
pChronicle sidecar that writes canonical Lance events. pVisor itself does
not open Lance. `--gateway-stream-markdown` can maintain live AgenticMD at
the same time. Markdown is a diagnostic projection. Dataset consumption
always goes through the pChronicle API and the `pchronicle` commands.

### Storyline three-table Lance

`StorylineLanceStore` is the normalized storage representation used for
analysis and ATIF interop: `runs.lance`, `steps.lance`, and
`tool_calls.lance`. It folds observation results onto tool-call rows by
`source_call_id`, and it switches the three table versions atomically
through the version tuple in `CURRENT`. UTF-8/JSON cells above the
threshold are offloaded by BLAKE3 content address into a shared
`objects.lance` and reused across Runs. The public schema and SQL results
stay the same; queries restore Blobs lazily only when they actually
reference a content column.

Temporary `steps` queries over ATIF objects, arrays, pretty JSON, and
JSONL/NDJSON, and over ACTF objects/arrays, also have a projection-aware
fast path: DataFusion first passes the required columns and safe
predicates; the reader uses a seeded visitor to skip unreferenced large
fields and build a narrow Arrow batch directly. JSONL/NDJSON is read
record-by-record under resource limits. Arrays use a structural scanner
that extracts one element at a time and a slice decoder. A single object
is decoded from the reader as a stream. `SELECT *`, the other tables, and
OpenAI-message fall back to full Storyline normalization. Protocol,
publication order, and execution bounds are in
[Storyline three-table Lance](storyline-lance.md).

## 4. Directory layout

Flat Story:

```text
storage/
└── agent_id/
    └── session_id/
        ├── events.lance/
        │   ├── _manifest.json
        │   └── segments/<epoch-writer>.lance/
        └── session_id.md
```

A Run that contains subagents:

```text
storage/
└── agent_id/
    └── root_session_id/
        ├── events.lance/          # manifest + writer segments，按 session_id 过滤
        ├── root_session_id.md
        └── agent-<id>.md
```

An independent Storyline analysis store uses `CURRENT`,
`generations/<id>/{runs,steps,tool_calls}.lance`, and a root-level shared
`objects.lance`. The required `schema_version` in `CURRENT` validates the
entire four-table physical layout before any table is opened. It does not
change the canonical event directory above.

System-generated AgenticMD uses the `{session_id}.md` filename and the
`<!-- persisting:block:{source} … -->` block structure. The reader also
accepts speaker-less blocks, the legacy `role/seq/session/agent` fields,
and ordinary Markdown body text.

## 5. Writes and consistency

1. An `EventRecord` is converted to Arrow rows before it enters Lance. One
   bounded micro-batch is one Lance append to the current epoch segment,
   then an exact version is published with a manifest CAS.
2. The hot path does not read old rows, row counts, or `event_id`. It does
   not deduplicate, index, compact, or vacuum.
3. A conflict between producer identity and write coordinates still
   appends. `payload_json` keeps the original claims. Physical
   `session_id` / `agent_id` come from the caller coordinates and take
   effect on replay. Projections take the last non-empty value for the
   remaining identity claims in append order and do not add a
   read-before-write, dedup, or index cost for the conflict.
4. `seq` is a producer-defined Storyline ordinal. The replay cursor uses
   the immutable physical append order.
5. Distinct Stories in a Run bucket share the manifest and epoch segments,
   but replay and stats isolate by `session_id`.
6. Live Markdown locates blocks by `call_id + source` (with a legacy-role
   alias) so a streaming agent can update in place.
7. Canonical append and derived projections report results separately. A
   projection failure must not be presented as a durable event write.
8. Segment/manifest publication for distinct root URIs in one micro-batch
   is at most 16-way parallel. The same URI stays serial in batch order,
   so a single Story's physical append order is not relaxed.

The event fact layer provides at-least-once append. It does not provide
exactly-once delivery or ID uniqueness. Truncate, overwrite, and retry
dedup are not part of the fact write path. Conversion into an existing Run
fails; trimming should create a new Run or happen on a derived Storyline.
Compaction, a `session_id` index, and vacuum are explicit offline
maintenance.

The upper Run lease produces a monotonic epoch.
`EventWriterFence(epoch, writer_id)` is activated before a new writer
writes data. Readers only see the segment versions pinned by the
manifest, so an underlying append finished by an old writer after takeover
is invisible. Another `writer_id` in the same epoch is rejected. The
protocol provides writer fencing. Concurrent multi-writer merge is not a
supported write mode.

## 6. Format conversion

Generic peripheral formats convert through the Storyline hub:

```text
AgenticMD ─┐
ATIF ──────┼── Storyline ── events / AgenticMD / ATIF / ACTF / OpenAI messages
ACTF ──────┤
OpenAI msg ┘
```

Paths that must keep the original payload read and write events directly
and must not go through a lossy Storyline roundtrip. Peripheral exchange
formats are handled by `pchronicle import/export`.

## 7. Component boundaries

| Component | Owns | Does not own |
|---|---|---|
| Gateway | protocol parsing, call lifecycle, capture order, live projection policy | generic store, format schema, offline conversion |
| pChronicle | formats, paths, durability, reads, conversion, and revision lineage | network forwarding, Agent lifecycle |
| pVisor | Run lifecycle and Gateway / OverlayNet / OverlayFS assembly | long-lived run-data schema |

## 8. Related documents

- [Recorded data, views, and versions](../concepts/facts-and-projections.md)
- [Discover and query](../guides/discover-and-query.md)
- [Snapshot](catalog.md)
- [AgenticMD format](../reference/agenticmd.md)
- [Gateway architecture](../../pvisor/design/gateway.md)
- [pVisor CLI](../../pvisor/reference/cli.md)
- [`pchronicle` Dataset commands](../reference/cli.md)
