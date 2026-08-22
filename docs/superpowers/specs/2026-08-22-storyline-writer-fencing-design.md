# Storyline Cross-Process Writer Fencing Design

**Date:** 2026-08-22

## Goal

Make every mutation of an existing Storyline Lance generation safe when two
processes write the same Store concurrently. A committed `CURRENT` snapshot
must always refer to mutually consistent `runs`, `steps`, `tool_calls`, and
`objects` versions. A crashed or superseded writer must never publish after a
new writer takes ownership.

## Problem

`StorylineLanceStore` currently serializes writers with a process-local mutex
and, for local paths, an advisory file lock. Object-store writers from separate
processes are not serialized. They can read the same `CURRENT`, then mutate the
same three Lance datasets concurrently.

Each Lance merge retries conflicts independently against the newest table
version. One logical writer can therefore finish with a `runs` version that
contains another writer's rows, a `steps` version that does not, and a
`tool_calls` version that contains them again. The final compare-and-swap of
`CURRENT` chooses one tuple of version numbers but cannot repair mutations that
already diverged across the three datasets.

The macOS failure in
`independent_replacements_conflict_at_current_cas_and_retry_cleanly` is the
observable result: a losing Storyline has a tool-call row but no corresponding
step row in the published snapshot.

## Non-goals

- Do not weaken Storyline reconstruction or foreign-key validation.
- Do not retry, ignore, or platform-disable the failing concurrency test.
- Do not change Storyline public document, storage, or query APIs.
- Do not add a general distributed-lock service for unrelated pChronicle
  stores.
- Do not enter TTAS, Queue/Sampler, Search, or `persisting-dlcapt`.
- Do not make ordinary successful replacement copy the complete Store into a
  new physical generation.

## Single authoritative control object

Lease state and committed snapshot state must live in one CAS-managed object.
Using a separate lease object would leave a time-of-check/time-of-use race: an
old writer could validate its lease, a new writer could take over, and the old
writer could still publish `CURRENT` before the new writer publishes.

`CURRENT` becomes a control envelope:

```json
{
  "control_version": 1,
  "revision": 17,
  "committed": {
    "schema_version": 2,
    "generation": "...",
    "parent_generation": "...",
    "table_generation": "...",
    "runs_version": 4,
    "steps_version": 4,
    "tool_calls_version": 4,
    "objects_version": 2
  },
  "lease": {
    "epoch": 8,
    "owner_id": "process-random-id",
    "issued_at_unix_ms": 1787400000000,
    "expires_at_unix_ms": 1787400060000,
    "base_generation": "..."
  }
}
```

`committed` is the only state visible to readers. An active lease does not
change query results. `revision` advances on every lease acquire, renewal,
takeover, release, or publication. `epoch` advances only when a new owner
acquires or takes over the lease. `base_generation` records the committed
generation against which the writer began.

Existing Stores whose `CURRENT` contains a direct
`StorylineSnapshotPointer` remain readable. The first mutation interprets the
legacy pointer as `committed` with revision and epoch zero, then atomically
rewrites it as the control envelope. No offline migration is required.

An absent `CURRENT` remains an empty Store. Initial Store creation already
writes a unique physical generation, so competing creators continue to stage
independently and use create/update CAS to publish exactly one result. They do
not need the existing-generation lease.

## Lease acquisition and renewal

Before any replacement, incremental projection sync, or maintenance operation
mutates an existing physical generation, it must acquire the lease by a
conditional update of the control object.

Acquisition has three outcomes:

1. no active lease: increment the epoch, install the caller as owner, and
   continue on the committed physical generation;
2. unexpired lease owned by another writer: return a stable Storyline commit
   conflict before changing any Lance table;
3. expired lease: conditionally replace it with a higher epoch and mark the new
   writer as a takeover writer.

The lease uses a bounded TTL and a background renewal task. Renewal performs a
conditional control-object update and preserves owner and epoch. The writer
records lease loss if renewal observes another owner, another epoch, a changed
base generation, or an expired lease. Lease loss prevents publication.

The clock model matches the existing pChronicle Run lease: timestamps use Unix
milliseconds and object-store CAS provides serialization. Clock skew can cause
an early takeover and wasted work, but cannot allow two snapshots to publish,
because ownership and publication are fenced through the same control object.

## Normal replacement

An ordinary lease owner follows the existing incremental path:

1. pin `committed` and acquire its lease;
2. split and externalize incoming Storylines;
3. update the pinned physical generation's tables;
4. stop renewal and atomically update the control object, requiring the same
   owner, epoch, and base generation;
5. install the new committed snapshot and clear the lease in the same CAS.

If table writing fails, the owner conditionally clears only its own lease and
leaves the committed snapshot unchanged. Unpublished Lance versions may remain
for later maintenance, but readers stay pinned to the previous exact versions.

If final publication loses CAS, the operation returns commit conflict. It must
not clear a lease belonging to another owner or rewrite `CURRENT` from a stale
copy.

## Expired-lease takeover

An expired lease means the previous writer may still be running and may still
mutate the old physical generation. A takeover writer must therefore not reuse
that generation.

The takeover path:

1. pins the exact table versions in `committed`;
2. streams those pinned rows into a new unique physical generation;
3. applies the requested replacement to the new generation;
4. publishes the new generation only while still owning the takeover epoch;
5. clears the lease in the same publication CAS.

The stale writer can continue touching only the old generation. Its control
object CAS is fenced by the newer epoch and revision, so it cannot publish. The
new writer's tables are isolated from those stale mutations.

The shared `objects.lance` dataset remains content-addressed. Concurrent stale
additions may leave unreachable objects but cannot change a value already
bound to its content hash. The takeover snapshot must pin an objects version
that contains every object referenced by its new rows. Existing object
integrity checks continue to fail closed on missing, length-mismatched, or
hash-mismatched content.

Takeover is intentionally more expensive than a normal replacement. It is a
crash-recovery path, not the steady-state write path.

## Mutation coverage

Every operation that can advance versions in an existing Storyline physical
generation must use the same controller:

- `replace_storyline` and `replace_storylines`;
- streamed replacement and projected streamed replacement;
- incremental projection synchronization;
- Storyline maintenance that publishes compacted or reindexed versions.

Fresh creation and full rebuild already use unique physical generations. They
retain isolated staging and publish through the control object CAS, but do not
need to mutate a shared generation under the lease.

## Internal boundaries

The protocol lives in a new private `store/storyline/writer_control.rs` module.
It owns:

- control-envelope and lease wire types;
- legacy `CURRENT` decoding;
- conditional acquire, renew, publish, release, and takeover transitions;
- a lease guard that stops renewal on every exit path.

`store/storyline/mod.rs` owns the high-level replacement state machine and asks
the controller for authority; it does not implement CAS details. The existing
row encoding and Lance mutation code remains in `mutation.rs` and receives no
lease wire types.

No writer-control type is exported from `storage.rs` or the crate public
facade.

## Error semantics

- Active competing writer: `Storyline commit conflict`.
- Lost or expired ownership during renewal/publication: `Storyline writer lease
  lost` with no owner identifier or backend credential in the public message.
- Malformed or unsupported control envelope: fail closed while opening the
  Store.
- Failed takeover clone: leave `committed` unchanged and conditionally release
  only the takeover owner's lease.
- Failed lease release after an earlier operation error: preserve the original
  operation error and attach release failure as diagnostic context.

## Cleanup

Failed fresh generations and failed takeover generations are unreferenced and
may be deleted immediately when ownership is still known. If cleanup is
ambiguous, leave them for existing maintenance rather than risk deleting a
published generation. Lease records contain no credentials and are cleared by
successful publication or owner-checked release.

## Testing

Required tests are deterministic and use the existing shared-memory object
store:

- a second replacement is rejected before any table mutation while a live
  lease is held;
- the current independent-replacement test publishes one complete winner and
  no partial loser;
- renewal preserves owner and epoch while advancing control revision;
- an expired lease can be taken over with a higher epoch;
- takeover writes a new physical generation and preserves every committed
  Storyline not being replaced;
- a stale owner cannot publish or release after takeover;
- readers continue to see the previous committed snapshot while a lease is
  active;
- initial concurrent creation still produces one published projection and one
  `OutputNotEmpty` result;
- legacy direct `CURRENT` pointers remain readable and upgrade on first
  mutation;
- local file and shared-memory object-store targeted suites pass;
- the full `persisting-pchronicle` library test suite passes on macOS and Linux.

## Acceptance criteria

- No published snapshot can contain a run, step, or tool-call row introduced by
  a losing writer unless all corresponding rows from that same committed
  Storyline are present.
- A writer that loses its epoch cannot publish, renew, or release another
  writer's lease.
- Existing Stores open without migration and retain their committed generation.
- Normal successful replacements retain incremental table updates; full table
  copying occurs only for rebuild, initial creation, or expired-lease takeover.
- The fix remains private to Storyline storage and does not change public
  pChronicle APIs.
