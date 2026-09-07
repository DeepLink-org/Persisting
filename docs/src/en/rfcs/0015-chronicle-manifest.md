# RFC-0015: `chronicle.manifest` Dataset Sidecar

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Format name** | `chronicle.manifest` (TOML) |
| **Date** | 2026-09-07 |
| **Component** | `persisting-pchronicle`, `pchronicle` CLI, Warehouse explorer |
| **Implements** | `crates/persisting-pchronicle/src/store/chronicle_manifest.rs` |
| **Related** | [RFC-0014 Compact JSONL](0014-compact-jsonl.md) · [RFC-0013 path Directory](0013-pchronicle-warehouse-catalog.md) · [RFC-0001 Storyline](0001-storyline-format.md) |

---

## Summary

`chronicle.manifest` is a **pChronicle-owned TOML sidecar** placed at a Dataset
node root. It enables cheap discovery and stores common aggregate statistics so
Warehouse catalog / explorer paths do not need to open Lance datasets or scan
every record on every refresh.

This RFC uses RFC 2119 **MUST**, **MUST NOT**, **SHOULD**, and **MAY**.

## Motivation

Large local datasets (for example a compact-jsonl Lance directory with many
rows and `_offload/` objects) currently force discovery to `Dataset::open` and
force explorer acceleration to materialize per-record run summaries. With a
warehouse that mounts several such roots, five-second catalog refresh and tree
polling keep the serve process on high CPU even when the operator only browses
folder counts.

Lance's internal `_versions/*.manifest` is an MVCC control plane owned by Lance.
pChronicle MUST NOT encode application discovery or UI statistics there.

pChronicle already uses application control files for other layouts (`CURRENT`
for Storyline, `_manifest.json` for Events). `chronicle.manifest` extends that
pattern to a uniform, nestable Dataset node descriptor.

## Goals and non-goals

Goals:

- Define a stable on-disk file name, TOML schema, and nesting rules.
- Let discovery classify Dataset nodes by reading a small TOML file.
- Persist aggregate stats used by explorer tree / dataset summaries.
- Support nested Dataset trees by **automatically scanning** child directories for
  `chronicle.manifest`.
- Keep missing or stale manifests compatible with existing heuristic discovery.

Non-goals (v1):

- Extending Lance protobuf manifests.
- Replacing SQL or detailed run/record listing with the sidecar.
- Hand-maintained explicit `children` lists in parent manifests.
- Rewriting Storyline `CURRENT` or Events `_manifest.json` into this format
  (those remain authoritative for their layouts; optional future alignment).

## Terminology

- **Dataset node**: a directory that is either a physical source root (leaf) or
  a directory that aggregates nested Dataset nodes (branch).
- **Leaf**: a node whose `format` identifies a physical store (v1:
  `compact-jsonl/v1`).
- **Branch**: a node that exists to nest children; it has no physical `format`.
- **Fingerprint**: a string that binds `[stats]` to one physical revision so
  readers can detect staleness.

## File location and name

- The file name MUST be exactly `chronicle.manifest`.
- The file MUST live at the Dataset node root (sibling to Lance `data/`,
  Storyline `CURRENT`, etc., when those exist).
- Encoding MUST be UTF-8 TOML.

## Nesting and discovery

### Automatic child scan

Parents MUST NOT require an explicit children list. Discovery MUST:

1. If the current directory contains `chronicle.manifest`, parse it.
2. If `kind = "leaf"`, treat the directory as one source candidate for
   `format` and MUST NOT recurse into its interior for additional sources.
3. If `kind = "branch"`, scan **immediate** child directories only; for each
   child that contains `chronicle.manifest`, treat that child as a nested
   Dataset node and continue according to that child's kind.
4. If the current directory has no `chronicle.manifest`, keep the existing
   heuristic discovery, but when a subdirectory contains
   `chronicle.manifest`, prefer that node and MUST NOT open Lance solely to
   classify it.

Symlinks MUST be ignored. Existing `max_entries` / `max_files` limits still
apply to traversal.

### Branch aggregation and trajectory counts

Branch nodes MAY omit `[stats]` and MUST NOT be treated as trajectory sources.
Only leaf nodes contribute trajectories.

When presenting Catalog / explorer folder totals (`run_count` /
`record_count`):

- Readers MUST compute a prefix total as the **sum of all descendant leaf**
  `[stats].record_count` (and `failed_count`) values discovered under that
  prefix.
- Intermediate branch directories contribute **0** of their own; they only
  define nesting.
- A leaf MUST NOT recurse for additional nested sources, so a physical leaf
  cannot double-count child leaves.

#### No ancestor write-back (write amplification)

Publishing or updating a leaf MUST update **only** that leaf's
`chronicle.manifest`. Writers MUST NOT rewrite ancestor branch manifests to
cache rolled-up totals. Branch files SHOULD remain descriptive only, for
example:

```toml
schema_version = 1
kind = "branch"
```

Rolled-up folder counts are a **read-side** concern.

#### Process-level refresh cache

Warehouse / Catalog MAY keep an in-process cache of discovered leaf stats and
prefix aggregates (for example on the existing catalog snapshot /
acceleration path) so periodic UI refresh does not re-open Lance or re-list
large trees. Cache entries SHOULD invalidate when a leaf `fingerprint` or
manifest mtime changes, or when a new `chronicle.manifest` appears under a
scanned prefix. Process cache MUST NOT replace on-disk leaf manifests as the
source of truth that travels with the dataset.

## TOML schema (v1)

### Required top-level fields

| Field | Type | Rules |
|---|---|---|
| `schema_version` | integer | MUST be `1` for this RFC |
| `kind` | string | MUST be `"leaf"` or `"branch"` |

### Leaf-only fields

| Field | Type | Rules |
|---|---|---|
| `format` | string | MUST be present for `kind = "leaf"`; v1 writers MUST use `compact-jsonl/v1` |

Unknown `format` values MUST be preserved by generic readers; format-specific
openers MAY reject unsupported values.

### `[identity]`

| Field | Type | Rules |
|---|---|---|
| `fingerprint` | string | MUST be present when `[stats]` is present; binds stats to a physical revision |

For compact-jsonl v1, fingerprint SHOULD be `lance:version:<N>` where `<N>` is
the published Lance dataset version after write.

### `[stats]`

| Field | Type | Rules |
|---|---|---|
| `record_count` | integer ≥ 0 | MUST be present for leaf compact-jsonl writers |
| `failed_count` | integer ≥ 0 | MUST be present; use `0` when unknown/none |
| `min_timestamp` | string | MAY be omitted |
| `max_timestamp` | string | MAY be omitted |
| `total_tokens` | integer ≥ 0 | MAY be omitted |

Additional stats keys MAY be added in later schema versions; v1 readers MUST
ignore unknown keys under `[stats]`.

### Example: leaf

```toml
schema_version = 1
kind = "leaf"
format = "compact-jsonl/v1"

[identity]
fingerprint = "lance:version:1"

[stats]
record_count = 12345
failed_count = 0
min_timestamp = "2026-01-01T00:00:00Z"
max_timestamp = "2026-09-07T01:00:00Z"
```

### Example: branch

```toml
schema_version = 1
kind = "branch"
```

## Write path

- `chronicle.manifest` is a **store-layer contract**. The only compact-jsonl
  publication exits are `CompactJsonlStore::publish_manifest` and
  `CompactJsonlStore::import_path`. CLI `import` and `sync` MUST go through that
  store API and MUST NOT invent a parallel sidecar writer.
- Compact JSONL `import` / successful republish / `sync` snapshot MUST write
  `chronicle.manifest` at the output dataset root.
- Writes MUST be atomic on local filesystems (write temp + rename into place).
- After a successful physical write, `fingerprint` MUST match the published
  revision and `[stats].record_count` MUST equal the published row count.
- If manifesto publication fails during `import_path`, the import MUST fail so
  a half-published contract is not exposed. For read-side `ensure_manifest`
  upgrades, failure MAY be logged while still opening the physical dataset.

## Read path and staleness

- When `fingerprint` matches the opened physical revision, readers MAY trust
  `[stats]` for explorer aggregates without scanning rows.
- When the file is missing, unreadable, or fingerprint mismatches, store-layer
  `ensure_manifest` SHOULD rewrite the sidecar in place; if that fails, readers
  MUST fall back to existing discovery / summary paths.
- Manifest stats MUST NOT be the sole authority for query correctness; SQL and
  record listing still read the physical store.

## Warehouse explorer implications

- Catalog refresh and `/api/explorer/tree` SHOULD use nested manifests for
  discovery and folder `run_count` / `record_count` aggregates when available.
- Folder `run_count` at any prefix MUST equal the sum of descendant leaf
  trajectory weights (manifest `record_count` when present), not the count of
  child dataset nodes.
- Detailed run/record pages MAY still open the physical leaf; this RFC does
  not require a full sidecar index of every record identity.

## Required unit tests (v1)

Implementations MUST cover at least:

1. **Nested discovery**: `warehouse/(branch)` → `team/(branch)` →
   `codex_jsonl/(leaf, N)` yields exactly one compact source at
   `team/codex_jsonl` with `record_count = N`, without opening Lance when the
   leaf manifesto is present.
2. **Sibling leaves**: two leaves under one branch with counts `A` and `B`
   yield two sources; tree root `run_count = A + B`; each child folder shows
   its own leaf total.
3. **Prefix roll-up**: under dataset scope, prefix `team` aggregates all
   leaves under `team/…`; prefix equal to a leaf path shows that leaf only.
4. **Leaf non-recursion**: a leaf directory that also contains a nested
   `chronicle.manifest` MUST NOT emit an additional source for the nested
   child.
5. **Write isolation (contract)**: updating one leaf's manifesto MUST NOT
   require changing parent branch files for counts to remain correct after
   rediscovery (read-side sum).

## Compatibility

- Datasets without `chronicle.manifest` remain valid. The first store open or
  heuristic discovery that confirms compact-jsonl SHOULD backfill the sidecar.
- Lance schema metadata `pchronicle.format = compact-jsonl/v1` remains the
  physical format marker; the sidecar does not replace it.
- Object-store URIs are out of scope for v1 writers; remote reads MAY be added
  later with the same schema. Nested branch scanning on object stores MAY use
  prefix listing plus exact-key reads of `chronicle.manifest`; v1 does not
  require S3 name-glob search.

## Alternatives considered

1. **Extend Lance `_versions/*.manifest`** — rejected: binary MVCC format,
   not owned by pChronicle, unsuitable for nesting and UI stats.
2. **Warehouse-only memory cache as the sole stats store** — rejected: does
   not travel with the dataset and resets on process restart. Process cache
   remains valid as a **refresh optimization** on top of on-disk leaf
   manifests.
3. **Explicit parent `children` lists** — deferred: automatic scanning matches
   directory trees and avoids stale child lists.
4. **Write rolled-up `[stats]` onto every ancestor branch** — rejected for
   v1: causes write amplification and stale parents when a single leaf is
   updated.
