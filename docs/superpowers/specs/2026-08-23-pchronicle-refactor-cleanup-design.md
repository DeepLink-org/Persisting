# pChronicle Refactor and Cleanup Design

**Status:** Execution baseline

**Date:** 2026-08-23

**Scope:** pChronicle, its Control protocol, pVisor/pPilot trajectory producers,
and the pChronicle CLI/server surfaces that directly consume them.

## Purpose

pChronicle keeps its existing architecture—canonical append-only events,
rebuildable Storyline projections, manifest fencing, and atomic Storyline
`CURRENT` publication—but converges all production writes onto one durable
append service before simplifying optional features.

The work is not measured by removed lines. It is successful when:

1. one production append implementation owns batching, fencing, admission,
   acknowledgement, and maintenance scheduling;
2. Run lease epochs participate in canonical event publication;
3. append semantics are explicit about order, duplicates, rejection, and
   indeterminate acknowledgement;
4. Control cannot write to a caller-selected path or URI;
5. optional behavior remains only when a reproducible benchmark or an active
   product contract justifies it.

## Scope Boundaries

The following remain out of scope:

- TTAS and tiered tensor memory;
- queue/sampler and their tests or documentation;
- search and Search CLI surfaces;
- the standalone `persisting-dlcapt` component;
- a new storage backend or a Vortex implementation;
- exactly-once append, `event_id` deduplication, or an ingest-receipt index;
- removal of the event manifest, Storyline three-table model, content object
  reader, or Storyline `CURRENT` pointer;
- a wholesale merge of RunControl and AttemptRegistry.

## Evidence Corrections Incorporated

The cleanup plan does not assume that Server source routing lacks evidence.
The repository contains a 2026-08-12 release result in which warm point-query
P95 improved from about 258 ms to 4.72 ms on a 113k-row, 211-source fixture.
The benchmark is not currently reproducible from a checked-in runner, so the
plan first makes it reproducible and reruns it against the current code. The
historical evidence is
`benchmark/langfuse-pchronicle-review/server-acceleration-results-2026-08-12.json`.

`write_revisions` has no in-repository production caller. Its only uses are its
definition, its module test, and the public re-export. The Web
`AnalysisRevision` type is unrelated.

`RunCommitRequest.event_high_watermark` likewise has no production value or
consumer: pPilot always writes `None`, and the only non-`None` value is a unit
test fixture. It is removed with the V3 contract instead of being used to
justify a new cursor subsystem.

## Decision Ledger

| Decision | Surface | Why |
|---|---|---|
| Keep | canonical events, Storyline projection, manifest CAS/fencing, `CURRENT`, content reader | these are correctness boundaries or established compatibility surfaces |
| Converge | Control/Gateway append, Run lease fencing, backpressure, ACK semantics, writable targets | one production path currently bypasses mechanisms that already exist |
| Simplify | two fence domains, named-target map, derived unknown-count cache | avoids duplicate identity, ACL, scheduler, and dual-schema state machines |
| Remove | unused revision catalog and `event_high_watermark` placeholder | no production writer/reader or caller |
| Measure, then decide | source routing and content externalization default | checked-in evidence exists or can be generated; outcome is workload-dependent |
| Defer | commit cursor, signed capability, Vortex backend, object-table format migration | no current consumer or threat/workload evidence pays for the extra authority |

## Architecture

```text
pVisor / pPilot / embedded Gateway
                |
                | non-empty EventRecord batch + writer context + named target
                v
       RawEventAppendService
       - one bounded admission policy
       - appender cache keyed by writer authority; per-root state inside
       - micro-batch + durable manifest publication
       - receipt / rejection / indeterminate outcome
       - metrics snapshot
                |
                v
        canonical events.lance
                |
                +---- replay in committed fact order
                |
                +---- asynchronous, rebuildable Storyline projection
                            |
                            v
                runs / steps / tool_calls / objects + CURRENT
```

`RawEventLanceStore::append_events` remains available for explicitly offline
batch use during migration, but no long-lived service path may call it.

## Canonical Append Contract

### Writer identity and fencing

Control protocol version 3 adds a storage-independent writer context:

```rust
pub struct TrajectoryWriterLease {
    pub run_id: String,
    pub attempt_id: String,
    pub lease_epoch: u64,
}
```

The Control server maps this type to pChronicle's `EventWriterFence`, deriving
the storage `writer_id` from `attempt_id`; producers never depend on the storage
type. A second producer-selected writer identifier would duplicate identity and
create a conflict case without adding fencing power.

The event manifest gains a format marker and a two-value fence domain:
`AutoEpoch` or `RunLease`. A manifest without the field is read as
`AutoEpoch`. Such a store may perform one CAS-protected handoff to `RunLease`.
After handoff, unfenced/default append is rejected for that event root.
This permits existing datasets whose storage epoch is numerically unrelated to
the Run lease epoch to migrate without pretending the two old counters match.

The handoff is a forward-only writer compatibility boundary. A pre-V3 binary
does not understand the domain field and must not reopen a migrated root for
writes; rollback is read-only for that root. V3 rejects manifest versions newer
than it understands. This is acceptable for the process-local, strict-version
sidecar protocol, but it must be explicit in release and rollback notes.

Control validates writer context against the current local Attempt record or
Run lease. pVisor therefore publishes its active Attempt to the trajectory
sidecar even when a separate orchestration registry also exists. A higher lease
epoch replaces the active Attempt and immediately fences the older writer.

This phase deliberately does not add a second opaque capability-token state
machine. The protocol is authenticated per process and restricted to loopback;
the required writer context is checked against durable Run/Attempt state and
then enforced again by the manifest fence. A signed transferable capability is
deferred until a cross-host or mutually untrusted producer protocol exists.

### Ordering without a new cursor

`EventRecord.seq` remains wire-compatible for this plan. Documentation and a
`producer_seq()` accessor define it as producer evidence, not replay order. A
later semver-major release may rename the Rust field to `producer_seq`.

Canonical replay order is the manifest's immutable segment order followed by
row order inside each segment. Existing `replay(session, offset, limit)` remains
a session-filtered view with a session-relative offset. Manifest `fact_rows` is
run-global because multiple sessions share one `events.lance`; it is not exposed
as a session cursor.

This phase deliberately adds no `CommitCursor`, cursor column, allocator, or
run-global replay API. Should a real follow/resume consumer appear, it needs a
separately reviewed, run-scoped cursor contract rather than an incorrect wrapper
over session offsets.

### Duplicates and acknowledgement

`event_id` remains opaque evidence and may repeat. pChronicle remains
at-least-once and does not add an `ingest_id` deduplication index.

Append returns one of:

```rust
pub enum DurableAppendOutcome {
    Committed(AppendReceipt),
    Rejected(AppendRejection),
    Indeterminate(AppendIndeterminate),
}
```

- `Committed` is returned only after manifest publication makes every record
  visible and includes the published fact version and manifest revision.
- `Rejected` is returned only when the system can prove no fact from the
  request was published—for example validation, capacity, unknown target, or a
  manifest fence conflict. Retry is safe with respect to duplicates, though a
  fenced writer must first obtain new ownership.
- `Indeterminate` covers timeout, transport loss, or lost acknowledgement after
  admission. It must not be treated as a definite rejection or blindly retried;
  a caller that retries accepts possible duplicate facts. pChronicle does not
  prescribe whether that caller reuses or advances producer-local `seq`.

This preserves RFC-0007's existing uncertainty and duplicate semantics without
adding exactly-once machinery or redesigning Gateway's sequence allocator.

### Resource policy

Initial service defaults are:

| Limit | Default |
|---|---:|
| queued records | 256 |
| maximum event bytes | 4 MiB |
| maximum queued/in-flight bytes | 64 MiB |
| maximum batch records | 256 |
| maximum batch bytes | 8 MiB |
| batch delay | 2 ms |
| durable acknowledgement timeout | 30 s |

Byte accounting uses an RAII reservation released on every terminal worker
path, including panic and channel closure. A caller may request a shorter
timeout but cannot widen the server maximum.

## Control Write Targets

Control protocol version 3 replaces caller-provided `storage` with a configured
target name. `pchronicle serve` receives a repeatable
`--control-write-target NAME` option. Only named Dataset mounts in this set are
writable; read-only Warehouse mounts never become writable implicitly.

For the single-root sidecar convenience path, `default` is registered and
writable automatically. pPilot, which may use separate control and trajectory
roots, starts the process with `default=<control-root>` plus a named trajectory
mount and selects that name in its append request.

There is no per-agent ACL in this phase. Target existence, route identity
agreement, and path/URI confinement are sufficient for the authenticated
loopback protocol.

## Identity Rules

The server-selected target and request route are canonical. Producer
`session_id` and `agent_id` claims may be absent, but when present they must
match the canonical route. Version 3 rejects conflicts rather than storing two
different identities for one fact.

Historic rows retain their original payload and continue to replay with the
physical route. No migration rewrites canonical facts.

## Optional-Feature Cleanup

### Server acceleration

The current source-routing implementation is retained until a reproducible
current benchmark exists. The checked-in runner must report cold build time,
warm point/list/aggregate P50/P95, candidate source count, and RSS.

Always split run summaries from source routing. Then evaluate each routing
surface separately:

- retain identity/point routing only if warm point P95 is at least 2x faster
  than the unaccelerated path and no greater than 10 ms;
- retain partition/list or aggregate routing only if that query class is at
  least 2x faster than its unaccelerated path;
- any retained routing must keep cold index build at or below 250 ms and
  incremental RSS at or below 128 MiB on the 113k-row fixture;
- remove every routing map and rewrite branch that does not pass its own gate;
  keep the run-summary cache regardless.

These are decision budgets derived from the existing release result, not claims
about the current implementation. No task deletes routing before the current
A/B gate is evaluated.

### Foreign unknown fields

Readers continue accepting legacy `_storyline` envelopes. Writers preserve
same-format unknown fields. Cross-format foreign preservation becomes explicit
through `DocumentCodecOptions::preserve_foreign_unknowns` and the CLI flag
`--preserve-foreign-unknowns`.

Without the flag, an export that would lose foreign unknown fields fails with a
clear error; it never silently drops them. `unknown_key_counts` becomes a
non-authoritative compatibility cache that is recomputed at validation and
persistence boundaries. This plan does not remove the public JSON field or the
Lance column: doing so would add a wire break, a schema version, and dual readers
for negligible storage savings. Physical removal waits for an already-required
semver/storage-schema migration.

Default unknown-field budgets become finite: 10,000 logical fields and 16 MiB
of encoded unknown values per document. Existing fixtures must remain within
the defaults.

### Storyline content offload

The content-addressed implementation and reader remain. Inline is the candidate
new default; explicit externalization retains the existing 64 KiB threshold.
The default does not change until the benchmark gate below passes. This phase
does not remove `objects.lance` from `CURRENT` or stop creating the objects
dataset, because that is a separate storage-format migration.

Before changing the default, the benchmark records payload-size distribution,
write time, full-read time, projected-read time, and store bytes for inline and
externalized modes. If inline mode exceeds 1.25x either full-read or
projected-read latency, or exceeds 1.5x store bytes on the large-payload
fixture, externalization remains the default and only the configuration surface
is added.

### Revision catalog

`revision.rs` is removed together with its public re-export and isolated test.
RFC-0005 is marked Withdrawn/Unimplemented in the same change. Existing
`revisions.lance` files are not deleted or migrated.

### Vortex and S3 defaults

RFC-0006 is reduced in place to an experimental charter containing scope,
correctness gates, benchmark gates, go/no-go criteria, and deletion criteria.
No Vortex code or dependency is added.

The pChronicle library default becomes `lance-store`; S3 remains available via
explicit `s3-store`. The CLI keeps its explicit S3 feature. CI runs both the
minimal Lance build and the explicit S3 integration target.

## Rollout Order

1. Freeze feature growth and capture baseline tests/benchmarks.
2. Add protocol V3 types and manifest legacy-handoff support.
3. Build the shared append service with fencing, durable receipts, byte budgets,
   timeout classification, and metrics.
4. Wire Control, Gateway, pVisor, and pPilot; remove service-path direct append.
5. Move Control to named writable targets and enforce route identity.
6. Run crash-window and stale-writer tests; make V3 the only spawned protocol.
7. Rebaseline optional features and execute only the cleanup decisions whose
   gates pass.

P0 and P1 are serial because they alter the same wire and durability path. The
optional-feature cleanup changes are independent after the main-chain gate and
must ship as separate commits so each can be reverted alone.

## Acceptance Criteria

- 10,000 one-record sidecar requests under one writer lease reuse one manifest
  writer epoch and keep visible segment count bounded by the configured seal
  and maintenance policy rather than event count.
- After takeover, the old writer receives a fenced rejection and cannot advance
  manifest fact state; the new writer can commit.
- Every committed response identifies the published fact version and manifest
  revision at which all accepted records are already replayable.
- Oversize events and exhausted byte budgets are rejected before admission;
  any timeout after transmission/admission is reported as indeterminate unless
  the service can prove publication did not occur.
- Control rejects unknown targets, non-writable mounts, and conflicting route
  identity without touching storage.
- Unit crash-window tests prove a private/unmanifested segment is invisible;
  process tests kill before reading an append response and after receiving a
  committed response. Restart preserves the documented ACK semantics in both
  cases.
- Existing events datasets remain readable; the first V3 append performs at
  most one explicit legacy fence-domain handoff, after which pre-V3 writers are
  unsupported for that root.
- Storyline projection, `CURRENT`, content hydration, and canonical replay tests
  continue to pass.
- Optional cleanup is performed only after its benchmark or compatibility gate
  is recorded in the repository.
