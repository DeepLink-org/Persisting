# pChronicle Canonical Event Format v2 Design

Status: Approved in brainstorming (revised after design review)
Date: 2026-08-17
Scope: pChronicle canonical event storage, Storyline projection, and trajectory query surfaces

Revision note (2026-08-17): the normalized identity was changed from a single
encoded `storyline_id` key to a natural `(attempt_id, storyline_id)` column
pair. The `attempt_storyline_v1` codec is retained but demoted to a physical
layout-key role (`canonical_key`); it no longer appears in normalized tables,
joins, or query surfaces, and no external field names are renamed. See
"Alternatives considered" for the rejected single-key formulation.

## 1. Summary

[FRAME, HIGH] pChronicle canonical v2 stores every accepted runtime event as an
append-only fact, and isolates repeated Attempts by making
`(attempt_id, storyline_id)` the logical Storyline grouping pair:
`attempt_id` and the original `storyline_id` are first-class natural columns,
and producer `session_id` is metadata that never routes, partitions, or joins.

[FRAME, HIGH] Attempt is not a separate storage entity or transaction
boundary. Where a single flat name is required — physical layout names,
lineage source identity, and incremental-sync affected keys — the codec
`attempt_storyline_v1` encodes the pair into a canonical layout key of the form
`a:<encoded-attempt-id>::s:<encoded-storyline-id>`; an event without an
Attempt uses `u::s:<encoded-storyline-id>`. The encoded key is a physical
naming artifact stored as `canonical_key`; it is not the logical identity and
is not the normalized join key.

[FRAME, HIGH] The format is intentionally breaking. The project has not been
released, so v2 does not read, migrate, or silently reinterpret older developer
datasets.

## 2. Goals

- [FRAME, HIGH] Prevent events from different Attempts from collapsing into one
  normalized Storyline.
- [FRAME, HIGH] Remove `session_id` from routing, partitioning, grouping, and
  normalized primary-key semantics.
- [FRAME, HIGH] Preserve the producer's original `EventRecord` envelope while
  keeping canonical routing coordinates explicit and queryable.
- [FRAME, HIGH] Preserve existing external adapter and `StorylineDocument`
  field names; identity cleanup happens in storage internals and normalized SQL
  surfaces only.
- [FRAME, HIGH] Keep Attempt-wide analytical queries on an indexed natural
  column rather than string-prefix parsing.
- [FRAME, HIGH] Keep append admission permissive: missing optional identities,
  duplicate facts, and malformed timestamps should not unnecessarily stop
  capture.
- [FRAME, HIGH] Give the physical schema and manifest an exact version boundary
  so incompatible data fails closed.
- [FRAME, HIGH] Make projection ordering deterministic within a scoped
  Storyline and make incremental projection lineage sensitive to identity and
  ordering rules.
- [FRAME, HIGH] Report partial success explicitly when one append call spans
  multiple Run partitions.

## 3. Non-goals

- [FRAME, HIGH] No Attempt-level directory, manifest, segment, or transaction
  hierarchy; Attempt is a nullable column, not a storage entity.
- [FRAME, HIGH] No rename of `StorylineDocument` fields, external adapter field
  names, or wire formats. The hub format gains an optional `attempt_id` field
  and is otherwise untouched by identity scoping.
- [FRAME, HIGH] No `event_id` generation, uniqueness constraint, deduplication,
  or idempotency protocol.
- [FRAME, HIGH] No event-versus-append-context identity conflict detector. The
  producer and append caller are required to use the same event contract.
- [FRAME, HIGH] No v1 compatibility reader, migration command, or automatic
  repair of existing developer datasets.
- [FRAME, HIGH] No Vortex storage conversion or projection acceleration.
- [FRAME, HIGH] No changes to TTAS, Queue, Search, or standalone dlcapt.

## 4. Motivation and current-state findings

[KNOWN, HIGH] `EventIdentity.seq` is defined at Attempt scope, and
`EventIdentity` already carries optional `run_id`, `attempt_id`, and
`storyline_id` values.

[KNOWN, HIGH] The current pChronicle raw Arrow schema indexes `session_id` but
does not index `run_id`, `attempt_id`, or `storyline_id`. The current Storyline
projector reconstructs a grouping key from overloaded identity fallbacks.

[INFERRED, HIGH] When two Attempts reuse the same session or Storyline claim,
that model cannot reliably keep their event histories separate. Because
`call_id` association occurs after grouping, the collapse can also associate
same-named calls across Attempts.

[KNOWN, HIGH] The current event manifest has no schema or format marker. The
segment reader compares Arrow schemas, but a manifest does not declare which
canonical contract produced its segments.

[KNOWN, HIGH] The current public batch append can publish successful Run
partitions and then return an overall error because another partition failed.

[INFERRED, HIGH] These findings justify an explicit canonical identity, a
versioned manifest, and a partition-aware result without introducing Attempt
as a new physical storage layer.

## 5. Identity model

### 5.1 Boundaries

- [FRAME, HIGH] `Run` remains the storage root, writer-fence scope, manifest
  scope, segment-publication scope, and smallest append atomicity boundary.
- [FRAME, HIGH] `Storyline` is the projection and query identity, represented
  by the natural pair `(attempt_id, storyline_id)` where `attempt_id` is
  optional.
- [FRAME, HIGH] `Attempt` is a persisted nullable logical column and one input
  of the canonical layout-key codec. It is not a storage entity, directory
  hierarchy, or transaction boundary.
- [FRAME, HIGH] `session_id` is optional producer metadata only.

[FRAME, HIGH] The append boundary uses an unambiguous context instead of
overloaded session coordinates:

```rust
struct EventAppendContext {
    agent_id: String,
    run_id: String,
    storyline_id: String,
}
```

[FRAME, HIGH] `agent_id` and `run_id` are required. The event's non-empty
`identity.storyline_id` is the original Storyline identity; when it is absent or
empty, the append context's non-empty `storyline_id` supplies it. If neither is
available, that event is rejected.

[FRAME, HIGH] The event's `identity.attempt_id` supplies Attempt identity. A
missing or empty Attempt selects the unscoped form. Other Attempt and Storyline
strings are opaque: they are not trimmed, parsed for application meaning, or
otherwise normalized before encoding.

### 5.2 Codec

[FRAME, HIGH] The canonical layout key uses these exact forms:

```text
scoped:   a:<percent-encoded-attempt-id>::s:<percent-encoded-storyline-id>
unscoped: u::s:<percent-encoded-storyline-id>
```

[FRAME, HIGH] The codec applies percent-encoding to UTF-8 bytes. ASCII letters,
digits, `-`, `.`, `_`, and `~` pass through; every other byte becomes uppercase
`%HH`. The fixed `a:`, `u::s:`, and `::s:` markers are never part of encoded
input.

[COMPUTED, HIGH] Because `:`, `%`, and all non-ASCII bytes are encoded, input
containing the delimiter or resembling a canonical prefix cannot collide with
the structural markers.

[FRAME, HIGH] Encoding and decoding live in one shared helper. Callers do not
construct keys with string concatenation. The encoded form is used only for
physical layout names, lineage source identity, incremental-sync affected-key
sets, and the physical `canonical_key` routing column. It does not appear in
normalized tables, adapter surfaces, or SQL predicates.

[FRAME, HIGH] The codec is named `attempt_storyline_v1`. Canonical format v2
pins that codec version; changing the grammar or escaping rules requires a new
format version and projection recipe.

### 5.3 Consequences

- [COMPUTED, HIGH] The same original Storyline in different Attempts yields
  different canonical layout keys and therefore different normalized
  Storylines.
- [COMPUTED, HIGH] Events with the same original Storyline and no Attempt share
  one unscoped Storyline.
- [COMPUTED, HIGH] The design cannot later separate two unknown Attempts that
  were both ingested without `attempt_id`; the missing distinction is not
  reconstructable.
- [FRAME, HIGH] Queries centered on a known Storyline filter the natural
  `storyline_id` column (with `attempt_id` when scoped). Attempt-wide
  analytical queries filter the indexed `attempt_id` column; no prefix parsing
  or percent-encoding is required in any query surface.

## 6. Canonical physical schema

[FRAME, HIGH] Canonical format v2 uses the following exact Arrow field order,
types, and nullability:

| Ordinal | Field | Arrow type | Nullable | Meaning |
|---:|---|---|:---:|---|
| 0 | `seq` | `UInt64` | no | Producer sequence within the Attempt |
| 1 | `event_id` | `Utf8` | yes | Opaque producer/business identity |
| 2 | `timestamp` | `Timestamp(Millisecond, UTC)` | no | Resolved canonical event time |
| 3 | `run_id` | `Utf8` | no | Canonical Run routing identity |
| 4 | `storyline_id` | `Utf8` | no | Original Storyline identity (natural column) |
| 5 | `attempt_id` | `Utf8` | yes | Original Attempt identity (natural column) |
| 6 | `canonical_key` | `Utf8` | no | Codec-encoded layout/routing key over `(attempt_id, storyline_id)` |
| 7 | `kind` | `Utf8` | no | Runtime event kind |
| 8 | `source` | `Utf8` | no | Runtime event source |
| 9 | `agent_id` | `Utf8` | no | Canonical storage agent |
| 10 | `session_id` | `Utf8` | yes | Producer session metadata |
| 11 | `call_id` | `Utf8` | yes | Call correlation index |
| 12 | `trace_id` | `Utf8` | yes | Trace correlation index |
| 13 | `parent_call_id` | `Utf8` | yes | Parent-call correlation index |
| 14 | `model` | `Utf8` | yes | Model filter derived from payload |
| 15 | `payload_json` | `Utf8` | no | Original producer `EventRecord` JSON |

[FRAME, HIGH] `turn_id`, `producer`, `parent_uuid`, `subagent_id`,
`parent_agent_id`, and `branch` remain in `payload_json`. They should become
physical columns only after a stable indexed-query requirement is demonstrated.

[FRAME, HIGH] `attempt_id` is denormalized from the producer identity for
direct indexed filtering and trivial projection. Row decoding validates the
invariant `canonical_key == encode(attempt_id, storyline_id)`; a mismatch
fails closed as a corrupt row.

[FRAME, HIGH] `seq` is `UInt64` to match the storage-independent event contract.
It is not converted to signed `i64` at the pChronicle boundary.

[FRAME, HIGH] `payload_json` is serialized before canonical coordinates or
fallback timestamps are applied. It therefore represents the producer
envelope received by pChronicle, modulo normal typed JSON serialization rather
than preservation of unavailable original wire bytes.

## 7. Admission and canonicalization pipeline

[FRAME, HIGH] Each event passes through this pipeline in order:

1. Validate that `source` and `kind` are non-blank after trimming for
   validation.
2. Serialize the received `EventRecord` to `payload_json` without mutating it.
3. Resolve the original Storyline identity from the event, falling back to the
   append context.
4. Read optional `attempt_id` from the event, store it as a natural column, and
   encode the canonical layout key.
5. Resolve the physical timestamp.
6. Copy physical `run_id` and `agent_id` from the append context.
7. Copy optional query fields from the producer record and derive `model` from
   payload.
8. Build the canonical `EventRow` and send accepted rows to the Run partition's
   append-only segment writer.

### 7.1 Timestamp resolution

[FRAME, HIGH] Timestamp candidates are attempted in this order:

1. `identity.timestamp_unix_ms` when it fits the physical `i64` millisecond
   representation;
2. the event's RFC3339 `timestamp` when it parses and is representable;
3. the append receive time.

[FRAME, HIGH] If both producer representations are valid but disagree, the
numeric value wins and a `timestamp_conflict` warning is recorded. An invalid
or out-of-range higher-priority candidate records a warning and falls through
to the next candidate. Timestamp defects do not reject an otherwise admissible
event.

## 8. Stored-event read model

[FRAME, HIGH] Decoding a physical row produces an internal value that preserves
both canonical coordinates and the original producer record:

```rust
struct CanonicalEvent {
    run_id: String,
    storyline_id: String,
    attempt_id: Option<String>,
    canonical_key: String,
    timestamp_ms: i64,
    agent_id: String,
    append_ordinal: u64,
    record: EventRecord,
}
```

[FRAME, HIGH] Row decoding validates that the denormalized non-routing fields
match `payload_json` where equality is expected. Canonical routing and timestamp
fields are carried outside the producer record instead of overwriting it.

[COMPUTED, HIGH] A record read and re-appended through the producer-event API is
encoded from its original `attempt_id` and original `storyline_id`, so a
canonical prefix is not recursively encoded.

[FRAME, HIGH] APIs that need canonical identity return `CanonicalEvent` or an
equivalent view. APIs that intentionally expose only the producer envelope use
an explicitly named extraction method; a generic `read_events ->
Vec<EventRecord>` must not imply that the returned record contains canonical
routing coordinates.

## 9. Storyline projection

[FRAME, HIGH] Projection operates on `CanonicalEvent`, not identity fallbacks
inside the raw `EventRecord`.

[FRAME, HIGH] Full projection performs these steps:

1. Read the pinned canonical fact snapshot in manifest append order and assign
   each row its stable append ordinal.
2. Group rows by `canonical_key` (equivalently by the natural pair
   `(attempt_id, storyline_id)`).
3. Stable-sort each group by `(record.seq, append_ordinal)`.
4. Build calls and turns within that group only.
5. Publish each normalized Storyline under its natural identity
   `(storyline_id, attempt_id)`; the encoded key names its on-disk layout.

[FRAME, HIGH] Duplicate `seq` values are valid appended facts. Append ordinal
breaks ties; timestamps do not determine logical order.

[COMPUTED, HIGH] Identical `call_id` values in different Attempts cannot be
associated because call reconstruction occurs only after grouping by the
Attempt-scoped canonical key.

[FRAME, HIGH] Incremental sync reads the new suffix to collect affected
`canonical_key` values, reads the full histories for only those keys,
reprojects them with the same ordering rule, and replaces only those normalized
Storylines.

[FRAME, HIGH] Projection lineage includes all of the following:

- canonical source URI;
- pinned `fact_version` and `fact_rows`;
- canonical `format_version = 2`;
- codec `attempt_storyline_v1`;
- grouping by `canonical_key` / `(attempt_id, storyline_id)`;
- ordering by `(seq, append_ordinal)`;
- projector recipe version.

[FRAME, HIGH] Any mismatch makes the projection stale and requires rebuild
rather than incremental sync.

## 10. Normalized model and query surfaces

[FRAME, HIGH] Normalized identity uses natural columns, and no external surface
is renamed:

- `runs`, `steps`, and `tool_calls` carry `storyline_id` (the original value)
  and a nullable `attempt_id`. Joins among them use
  `(storyline_id, attempt_id)` where Attempt scoping matters; `storyline_id`
  alone never merges two Attempts, and `attempt_id` alone never merges two
  unrelated Storylines.
- `StorylineDocument` keeps its existing `session_id` field name and value
  semantics (the logical conversation identity) and gains an optional
  `attempt_id` field. The mapping `StorylineDocument.session_id` ↔ normalized
  `storyline_id` is one-to-one and value-preserving.
- Parent and child Storyline links keep their current field names; child
  references resolve within the Attempt scope of the referencing document when
  an Attempt is present.
- Producer `session_id` remains raw-event metadata. Because the normalized
  identity column is named `storyline_id`, an accidental join between producer
  session metadata and normalized identity is a visible name mismatch rather
  than a silent wrong-value join.
- Exact Storyline lookup compares `(storyline_id, attempt_id)`; Attempt-wide
  analysis filters the indexed `attempt_id` column. No percent-encoding, codec
  helper, or prefix predicate appears in any SQL surface.
- External format adapters are unchanged: an ATIF `session_id` maps to
  `StorylineDocument.session_id` on import and back on export, exactly as
  today. External identifiers never pass through the canonical codec.

## 11. Append reports and diagnostics

[FRAME, HIGH] Batch append returns a partition-aware report:

```rust
struct EventAppendReport {
    accepted_records: usize,
    rejected_records: usize,
    warning_counts: BTreeMap<WarningCode, usize>,
    diagnostic_samples: Vec<EventDiagnostic>,
    partitions: BTreeMap<RunUri, PartitionOutcome>,
}
```

[FRAME, HIGH] Diagnostics have a stable code, input ordinal, optional
`event_id`, and concise detail. Warning and rejection counts cover the complete
batch. At most 32 diagnostic samples are retained, preventing report memory
from growing with a pathological batch.

### 11.1 Accepted with warning

- [FRAME, HIGH] conflicting valid timestamp representations;
- [FRAME, HIGH] malformed textual timestamp;
- [FRAME, HIGH] out-of-range numeric timestamp;
- [FRAME, HIGH] no producer timestamp;
- [FRAME, HIGH] missing `event_id`;
- [FRAME, HIGH] missing or empty `attempt_id`.

### 11.2 Rejected record

- [FRAME, HIGH] blank `source` after trimming for validation;
- [FRAME, HIGH] blank `kind` after trimming for validation;
- [FRAME, HIGH] no usable original Storyline identity after context fallback;
- [FRAME, HIGH] failure to serialize the typed producer event.

[FRAME, HIGH] Rejection is per record. Other admissible rows in the same Run
partition may still be published and are reported as accepted.

## 12. Publication, concurrency, and failure semantics

- [FRAME, HIGH] Every accepted input becomes one physical row, including
  duplicate `event_id`, `seq`, and payload values.
- [FRAME, HIGH] Duplicate `event_id` values are not detected or reported;
  neither the current batch nor stored history is scanned for uniqueness.
- [FRAME, HIGH] Append performs no read-before-write lookup or deduplication.
- [FRAME, HIGH] Within one Run partition, the accepted rows from a micro-batch
  become visible as one manifest publication unit.
- [FRAME, HIGH] A segment write or manifest CAS failure leaves none of that
  partition's new rows visible. Unreferenced private segment data is garbage
  eligible for the existing maintenance path.
- [FRAME, HIGH] Different Run partitions succeed or fail independently. The
  report records every outcome and does not convert known partial success into
  an ambiguous overall error.
- [FRAME, HIGH] Only a failure that prevents construction of a trustworthy
  partition report returns a top-level error.
- [FRAME, HIGH] Writer fencing remains epoch based. A stale writer cannot
  publish new visible rows even if it created private data.
- [FRAME, HIGH] Delivery remains at-least-once and append-only. Callers may
  retry failed partitions; pChronicle does not suppress duplicates.

## 13. Manifest and schema versioning

[FRAME, HIGH] Every canonical event manifest contains required
`format_version: 2`. New manifests are created with that value before any
segment is published.

[FRAME, HIGH] Opening a canonical source performs checks in this order:

1. decode enough manifest structure to require and inspect `format_version`;
2. reject every value other than `2` with `UnsupportedFormatVersion`;
3. validate manifest revision, fence, row counts, and segment invariants;
4. open pinned segment versions;
5. compare every segment's Arrow fields with the exact v2 schema.

[FRAME, HIGH] A missing version, unsupported version, malformed manifest,
missing pinned Lance version, or schema mismatch fails closed. The reader does
not infer a version from columns and does not attempt migration.

[FRAME, HIGH] Existing pre-v2 developer roots must be recreated explicitly by
their owners. The library does not delete or overwrite them automatically.

## 14. Testing and acceptance criteria

### 14.1 Codec unit tests

- [FRAME, HIGH] Round-trip ASCII, Unicode, `%`, `:`, `::s:`, and prefix-shaped
  input.
- [FRAME, HIGH] Prove scoped and unscoped forms cannot collide for the tested
  corpus.
- [FRAME, HIGH] Verify uppercase percent escapes and rejection of non-canonical
  encoded forms by the decoder.
- [FRAME, HIGH] Verify missing and empty Attempt values produce the unscoped
  form.

### 14.2 Schema and manifest tests

- [FRAME, HIGH] Assert exact field order, Arrow types, and nullability.
- [FRAME, HIGH] Assert `seq` supports the full `u64` event-contract domain.
- [FRAME, HIGH] Assert `attempt_id` and `canonical_key` columns exist with the
  declared nullability, and that decoding rejects rows whose
  `canonical_key != encode(attempt_id, storyline_id)`.
- [FRAME, HIGH] Assert a new manifest serializes `format_version: 2`.
- [FRAME, HIGH] Assert missing, lower, and higher versions fail before any
  segment is opened.
- [FRAME, HIGH] Assert a schema mismatch fails closed.

### 14.3 Admission and append tests

- [FRAME, HIGH] Cover scoped identity, unscoped identity, context Storyline
  fallback, and missing Storyline rejection.
- [FRAME, HIGH] Cover every timestamp priority, conflict, parse failure,
  overflow, and receive-time fallback.
- [FRAME, HIGH] Verify absent and duplicate `event_id` values append as separate
  facts.
- [FRAME, HIGH] Verify invalid records are rejected while valid rows from the
  same input batch are published.
- [FRAME, HIGH] Verify single-Run publication is all-or-none on storage failure.
- [FRAME, HIGH] Verify multi-Run partial success is represented per partition.
- [FRAME, HIGH] Verify diagnostic samples cap at 32 while counts remain exact.
- [FRAME, HIGH] Verify the append path performs no fact scan before writing.

### 14.4 Projection tests

- [FRAME, HIGH] Same original Storyline plus different Attempts yields two
  normalized Storylines.
- [FRAME, HIGH] Same original Storyline plus same Attempt yields one Storyline.
- [FRAME, HIGH] Missing-Attempt events for the same Storyline share one
  unscoped Storyline.
- [FRAME, HIGH] Events sort by `seq`; equal `seq` retains append order.
- [FRAME, HIGH] Equal `call_id` values in different Attempts do not associate.
- [FRAME, HIGH] Attempt-wide queries filter the indexed `attempt_id` column
  without string parsing or prefix predicates.
- [FRAME, HIGH] Incremental sync rebuilds only affected canonical keys.
- [FRAME, HIGH] Codec, format, or ordering-recipe drift invalidates lineage.
- [FRAME, HIGH] Reading and re-appending a producer record does not double
  encode the canonical key.

### 14.5 Query and adapter tests

- [FRAME, HIGH] Raw event filters use the natural `storyline_id` and
  `attempt_id` columns; `canonical_key` is maintained and validated but is not
  required in SQL.
- [FRAME, HIGH] Normalized tables join on `(storyline_id, attempt_id)` where
  Attempt scoping matters.
- [FRAME, HIGH] Producer `session_id` remains metadata and cannot alter
  routing or join to normalized identity.
- [FRAME, HIGH] External `session_id` adapters are value- and name-symmetric
  in both directions and never invoke the canonical codec.
- [FRAME, HIGH] `payload_json` deserializes to an `EventRecord` semantically
  equal to the producer record received at append.

### 14.6 Verification scope

[FRAME, HIGH] Acceptance uses targeted pChronicle build, library tests, and
trajectory CLI tests. Workspace-wide checks that pull in TTAS, Queue, Search,
or standalone dlcapt are not part of this change's acceptance criteria.

## 15. Alternatives considered

### 15.1 Encoded canonical key as the normalized identity (original draft)

[INFERRED, HIGH] The original formulation stored
`a:<attempt>::s:<storyline>` directly in a single normalized `storyline_id`
column. It was rejected in design review: the column name misdescribes a
composite routing key; Attempt-wide queries degrade to unindexed prefix
predicates; the codec leaks into every join and query surface; and the naming
collides with the producer's original `storyline_id` meaning. The natural
column pair keeps the codec's layout benefits without these costs.

### 15.2 Attempt-level physical storage hierarchy

[INFERRED, HIGH] `run/attempt/segment` gives stronger physical isolation, but
expands writer fencing, manifest publication, discovery, and small-segment
management. Attempt is not required to be an independent transaction boundary.

### 15.3 Keep `session_id` as the normalized key

[INFERRED, HIGH] This minimizes renames but preserves the semantic overload that
caused routing, producer metadata, and normalized identity to diverge. It was
rejected in favor of one explicit natural identity pair; producer `session_id`
survives unchanged as metadata, which keeps industry-facing surfaces stable
without reusing the overloaded word as a key.

## 16. Implementation boundary

[FRAME, HIGH] Implementation is expected to touch only in-scope event contract
documentation, pChronicle raw-event rows/schema/manifest/data source, append
reporting, Storyline projection/model rows, and their targeted tests and CLI
surfaces. External format adapters, `StorylineDocument` field names, and
wire formats are unchanged by this design.

[FRAME, HIGH] Implementation must not begin until this specification has been
self-reviewed, reviewed by the user, and converted into a separate stepwise
implementation plan.

## 17. Relationship to the document source convergence design

[FRAME, HIGH] This design is orthogonal to and composed with the
*pChronicle document source convergence design* (2026-08-17, commit
`4c063956`): the convergence design owns the read/model/facade axis
(Storyline as the authoritative trajectory model, `DocumentFormat`,
query capabilities, the four public modules), while this design owns the
write/identity/schema axis (append pipeline, identity columns, manifest
versioning). Both share the facts-versus-projections split: canonical
events are append-only facts; Storyline views are rebuildable projections.

[FRAME, HIGH] `EventAppendContext` and `EventAppendReport` are
storage-internal domain types, not wire DTOs. The cross-process append
protocol is defined solely by `persisting-events` (convergence design §11);
this design adds no protocol DTO to pChronicle.

[FRAME, HIGH] This design's public types live in the convergence design's
module layout: append context and report belong to `storage`; projection
lineage and snapshots belong to `query`. `QuerySnapshot::CanonicalEvent`'s
`format_version` is this design's manifest version (§13); legacy unversioned
manifests report `1`.

[FRAME, HIGH] Implementation is sequenced after convergence phases three
and four: this design's projection output is written into the convergence
design's Storyline three-table Lance, and `StorylineDocument.attempt_id` is
registered as an accepted increment of convergence design §4.1. The facade
and DTO deletions land first; this design is not implemented on the old
facade.
