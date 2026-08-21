# pChronicle Automatic Storyline Projection Design

**Date:** 2026-08-21

## Goal

Remove the public `pchronicle project` command and make canonical-events to
Storyline projection an automatic part of normal pChronicle workflows:

- `import` performs an explicit, one-shot projection when its input is exactly
  one canonical `events.lance` Store;
- `serve` builds, rebuilds, synchronizes, verifies, and discovers projections
  automatically;
- `status` reports projection health alongside Dataset health.

The canonical `events.lance` Store remains the source of truth. The Storyline
Lance Store remains a rebuildable derived representation and never becomes an
independent authority for canonical event facts.

## Non-goals

- Do not merge projection logic into the Gateway or Control append hot path.
- Do not change canonical event ordering, append acknowledgement, fencing, or
  physical storage semantics.
- Do not replace the existing Storyline three-table Store or its lineage model.
- Do not retain a hidden, deprecated, or compatibility `project` command.
- Do not expose manual projection lifecycle flags through `serve`.

## Public CLI

### One-shot import

An input that is exactly one canonical event Store uses this form:

```bash
pchronicle import \
  --from ./run/events.lance \
  --output ./run/storyline
```

`--from` accepts a local path or object-store URI for this mode. Detection must
open and validate the canonical event manifest; a directory name or `.lance`
suffix alone is not sufficient. If the input is not a canonical event Store,
the existing JSON, JSONL, NDJSON, directory-recursive, or stdin import behavior
applies unchanged.

Canonical-event import always writes a Storyline Lance projection. It does not
require `--format events` or `--output-format storyline`, and it does not copy,
move, or mutate the source Store. Internally, `--output-format` becomes optional:
omission still means `preserve` for JSON imports and means `storyline` for a
canonical event Store. Explicit `--output-format storyline` is also accepted;
explicit `preserve` is rejected for canonical events rather than silently
ignored. The destination remains create-only. A non-empty or existing
destination is a conflict, including a direct Storyline Store without matching
canonical lineage.

The successful response extends the existing import response boundary and
reports `format=events`, `output_format=storyline-lance`, `sources=1`, the
projected trajectory count, and `fact_rows`. `input_bytes` becomes optional: it
remains present with unchanged meaning for byte-backed JSON imports and is
omitted for a canonical event Store, where physical encoded size is not a
stable logical input measure. Canonical-event import does not claim
byte-preserving import.

### Removed command

These commands are removed without aliases:

```text
pchronicle project build
pchronicle project status
pchronicle project verify
pchronicle project sync
pchronicle project watch
pchronicle project rebuild
```

The underlying Rust projection operations remain available for internal reuse
by `import`, `serve`, `status`, and tests.

### Unified status

`pchronicle status` includes a `projections` array. Each entry contains:

```json
{
  "source_path": "run/events.lance",
  "projection_path": "run/storyline",
  "status": "fresh",
  "generation": "generation-id",
  "fact_version": 12,
  "fact_rows": 4812
}
```

The stable states are:

- `fresh`: lineage matches the current canonical fact snapshot;
- `stale`: a projection exists but its fact watermark is behind;
- `missing`: no projection exists at the deterministic destination;
- `error`: the source, projection, lineage, or current generation cannot be
  opened or verified.

Nullable generation and watermark members are omitted when unavailable. Table
output includes a compact projection summary; JSON output preserves the full
per-source records. Status is observational and never performs maintenance.

## Deterministic projection location

For every discovered canonical Source named `events.lance`, its automatic
projection is the sibling `storyline` Store:

```text
run/events.lance  ->  run/storyline
```

The same URI join rule applies to local filesystems and object stores. Multiple
canonical Sources in different directories therefore receive independent
sibling projections.

The destination is owned by automatic projection only when its committed
lineage identifies the matching canonical source. A pre-existing destination
without canonical lineage, with lineage for another source, or with malformed
state is never overwritten. Initial startup reports a conflict; a runtime
discovery records an error and retries without changing the destination.

## Serve startup

`serve` scans every mounted Dataset for canonical event Sources before
publishing its single readiness record. It processes Sources with bounded
concurrency and applies this state machine to each deterministic destination:

1. missing destination: build a complete projection from one pinned fact
   snapshot;
2. fresh matching projection: no-op;
3. stale matching projection with a valid append watermark: incrementally sync;
4. matching projection that requires rebuild: publish a complete new physical
   generation, then atomically switch `CURRENT`;
5. foreign, lineage-free, or malformed destination: fail closed.

Readiness is emitted only after every initially discovered canonical Source is
fresh. A projection failure therefore prevents Warehouse, Control, Gateway, or
combined `serve` modes from advertising readiness. A Dataset with no canonical
event Source requires no projection work.

Two `serve` processes may race on the same source. Publication continues to use
the existing generation and compare-and-swap contracts. A process that loses a
race reloads `CURRENT`; a matching fresh winner is success, while a conflicting
or malformed winner remains an error. No process mutates a published generation
in place.

## Runtime maintenance

After readiness, an internal projection supervisor periodically:

1. discovers newly created canonical event Sources;
2. reads each current canonical fact watermark;
3. incrementally synchronizes append suffixes when the existing proof
   obligations permit it;
4. performs a complete rebuild when lineage, recipe, or monotonic-watermark
   checks require one;
5. periodically verifies apparently fresh projections.

The worker uses bounded concurrency and capped exponential backoff. One Source
failure does not stop maintenance for other Sources.

Runtime projection work remains outside Gateway and Control append
acknowledgement. Canonical writes are durable when their existing append
contract succeeds; they do not wait for Storyline projection. A projection
failure after readiness is written to stderr and retained in supervisor state
for diagnostics, but it does not stop Warehouse, Control, or Gateway. Queries
continue to use the existing bounded canonical fallback when no fresh
projection is available.

Process shutdown stops new maintenance iterations, waits for an active atomic
publication boundary to settle, and then participates in the unified `serve`
shutdown. It never leaves `CURRENT` pointing at an incomplete generation.

## Warehouse Catalog refresh

Automatic projection supersedes the earlier rule that `serve` never refreshes
Warehouse automatically. After a successful projection build, sync, or
rebuild, `serve` constructs a complete new Catalog Snapshot for the mounted
Datasets. Only a fully successful Snapshot is atomically installed.

Snapshot construction failure leaves the previous Snapshot serving requests
and enters the same bounded retry path as projection maintenance. Queries do
not observe partially discovered Sources or half-published projections. Several
projection publications in one maintenance iteration are coalesced into one
Catalog refresh.

Gateway-only or Control-only `serve` modes still maintain projections but do
not construct an unused Warehouse Snapshot when `--listen` is absent. If a
Warehouse listener is enabled later only by starting a new process, its startup
scan establishes a fresh projection and initial Snapshot before readiness.

## Error and output boundaries

- `import` failures use the existing CLI boundary codes and never publish a
  partial destination.
- Startup projection failures prevent stdout readiness and are reported on
  stderr without secrets.
- Runtime maintenance failures never write machine events to stdout because
  stdout is reserved for the one `serve` readiness record.
- Control authentication tokens remain present only in readiness JSON and are
  never included in projection or Catalog diagnostics.
- `status` converts per-projection read or verification failures into stable
  `error` records; debug source chains remain behind the existing debug error
  boundary.

## Documentation and migration

Documentation presents only three normal lifecycle commands:

```text
pchronicle import ...
pchronicle serve ...
pchronicle status ...
```

Manual build/sync/watch/rebuild instructions are removed. Existing automation
using `pchronicle project` must migrate as follows:

| Old workflow | Replacement |
| --- | --- |
| `project build` | `import --from EVENTS_URI --output STORYLINE_URI` |
| `project status` / `project verify` | `status DATASET_URI` |
| `project sync` / `project watch` | automatic under `serve` |
| `project rebuild` | automatic under `serve` |

## Testing

Required tests cover:

- local and object-store canonical-event import;
- manifest-based input detection rather than suffix-only detection;
- create-only publication and protection of foreign or lineage-free outputs;
- startup build, incremental sync, automatic rebuild, and readiness ordering;
- runtime discovery of a newly created canonical Source;
- projection retry without blocking Gateway or Control durable writes;
- automatic Catalog refresh, atomic Snapshot switching, and retention of the
  previous Snapshot after refresh failure;
- multiple Sources and concurrent `serve` processes using existing CAS;
- `fresh`, `stale`, `missing`, and `error` status encoding;
- CLI help and command parsing proving `project` is absent;
- release smoke coverage for canonical-event import and automatic `serve`
  projection.
