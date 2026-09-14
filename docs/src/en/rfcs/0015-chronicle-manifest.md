# RFC-0015: `chronicle.manifest` Dataset Sidecar

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Format name** | `chronicle.manifest` (TOML) |
| **Date** | 2026-09-07 (updated 2026-09-11) |
| **Component** | `persisting-pchronicle`, `pchronicle` CLI, Warehouse Datasets UI |
| **Implements** | `crates/persisting-pchronicle/src/store/chronicle_manifest.rs` |
| **Related** | [RFC-0014 Compact JSONL](0014-compact-jsonl.md) · [RFC-0013 path Directory](0013-pchronicle-warehouse-catalog.md) · [RFC-0001 Storyline](0001-storyline-format.md) |

---

## Summary

`chronicle.manifest` is a **pChronicle-owned TOML sidecar** placed at a Dataset
node root. It enables cheap discovery and stores common aggregate statistics so
`pchronicle ls` and the Web Datasets page can classify a directory and show
type / trajectory-count previews without opening Lance or scanning every
record.

This RFC uses RFC 2119 **MUST**, **MUST NOT**, **SHOULD**, and **MAY**.

## Motivation

Large local datasets (for example a compact-jsonl Lance directory with many
rows and `_offload/` objects) currently force discovery to `Dataset::open` and
force the Datasets UI to reconstruct folders from run summaries. With a
warehouse that mounts several such roots, five-second catalog refresh and tree
polling keep the serve process on high CPU even when the operator only browses
one directory level.

Lance's internal `_versions/*.manifest` is an MVCC control plane owned by Lance.
pChronicle MUST NOT encode application discovery or UI statistics there.

pChronicle already uses application control files for other layouts (`CURRENT`
for Storyline, `_manifest.json` for Events). `chronicle.manifest` extends that
pattern to a uniform, nestable Dataset node descriptor.

## Goals and non-goals

Goals:

- Define a stable on-disk file name, TOML schema, and nesting rules.
- Let discovery classify Dataset nodes by reading a small TOML file.
- Persist aggregate stats used by `ls` / Web Datasets previews (type and
  trajectory count).
- Support nested Dataset trees by **automatically scanning** child directories for
  `chronicle.manifest`.
- Split **list** from **open**:
  - `list(path)` is shell-like: one immediate level of children. It MUST NOT
    flatten nested sources.
  - `open(path)` builds a Snapshot. A leaf Dataset is one source. A plain
    directory MAY be opened as a **virtual dataset** that unions all nested
    Dataset leaves and peripheral JSON under that path.
- When the sidecar is missing, still classify Datasets via `CURRENT` / events /
  compact-jsonl markers. `list` MUST NOT recursively list an entire object-store
  prefix just to classify. `open` of a directory MAY recurse under the existing
  `max_entries` / `max_files` bounds.

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
  `compact-jsonl/v1` or `storyline/v1`).
- **Branch**: a node that exists to nest children; it has no physical `format`.
- **Directory**: a path with no leaf Dataset marker. It is a navigational
  folder for `list`. It MAY still be `open`'d as a virtual dataset. This is not
  RFC-0013 path Directory (name → path + ACL).
- **list**: one-level listing of a path. CLI `pchronicle ls` and the Web Datasets
  page MUST share this operation.
- **open**: freeze a path as a Snapshot (`query` / `find` / `stats` / serve
  mount / Open in Runs).
- **Virtual dataset**: the Snapshot produced by opening a Directory: nested
  Dataset leaves plus JSON / JSONL / NDJSON that are not inside a leaf.
- **Fingerprint**: a string that binds `[stats]` to one physical revision so
  readers can detect staleness.

## File location and name

- The file name MUST be exactly `chronicle.manifest`.
- The file MUST live at the Dataset node root (sibling to Lance `data/`,
  Storyline `CURRENT`, etc., when those exist).
- Encoding MUST be UTF-8 TOML.

## Nesting and discovery

`list` and `open` share classification. They MUST NOT share result shape.

A path is a **leaf Dataset** when any of these holds, in this order:

1. `chronicle.manifest` with `kind = "leaf"`
2. `CURRENT` (Storyline)
3. `events.lance/_manifest.json` or a directory named `events.lance` with
   `_manifest.json`
4. compact-jsonl Lance (`pchronicle.format = compact-jsonl/v1`, or a leaf
   sidecar with `format = "compact-jsonl/v1"`)

Otherwise the path is a **Directory** (including `chronicle.manifest`
`kind = "branch"`).

Symlinks MUST be ignored. Existing `max_entries` / `max_files` limits still
apply to traversal. **`import` / `sync` use a separate recursive JSON scan**
and are not bound by `list`.

### `list(path)` — shell-like one level

`pchronicle ls PATH` and Web Datasets MUST list **immediate** children of
`PATH`, like a shell `ls`. They MUST NOT flatten nested sources into the
current listing.

Each child is one of:

| Child | `list` kind | Preview |
|---|---|---|
| Leaf Dataset directory | dataset | `format` plus `[stats].record_count` / `failed_count` when the sidecar has them |
| Directory | directory | name only; MUST NOT deep-scan for a preview |
| `.json` / `.jsonl` / `.ndjson` at this level | file | name; format MAY be unknown until `open` |

`list` of a **leaf Dataset** MUST NOT list Lance interiors (`data/`,
`_versions/`, `generations/`, `_offload/`, and similar). The listing is that
one Dataset plus its preview. Further drill-down is `open` / Runs, not
another `ls`.

`pchronicle ls --sources` MUST `open(path)` and print the Snapshot source
table (the virtual-dataset members). Default `ls` stays one-level children.
`--sources` MUST NOT change `list` itself.

Parents MUST NOT require an explicit children list in a branch manifest.
`list` of a branch or Directory inspects **immediate** children only.
A child with a Dataset marker is listed as a dataset; other directories
are listed as directories; loose JSON at this level is listed as a file.

### `open(path)` — Snapshot, including virtual datasets

`open(path)` builds the query Snapshot:

1. If `path` is a leaf Dataset, the Snapshot has one source `_file_ = "."`.
   Readers MUST NOT recurse into the leaf for additional sources, even if a
   nested `chronicle.manifest` exists inside it.
2. If `path` is a JSON / JSONL / NDJSON file, the Snapshot has one file source.
3. If `path` is a Directory, the caller MAY **force it as a query root**.
   The Snapshot is a **virtual dataset**: recurse under `path` and collect
   every nested leaf Dataset and every JSON / JSONL / NDJSON that is **not**
   inside a leaf. Intermediate unlabeled directories are not sources; they
   only contribute path segments to `_file_`. `_file_` is relative to this
   `path`.

A serve mount is one `open(uri)`. Browsing inside that mount is `list` of a
prefix. **Open in Runs** on a prefix SHOULD keep that Snapshot and MAY narrow
with `_file_`. The Runs page MAY still reconstruct a path tree from run
summaries (`PathExplorer`); that tree is not Datasets `list`.
`pchronicle query ./warehouse/team` is a separate `open` of that URI.

Branch manifests still mean "this node nests children". `open` of a branch
or of a Directory without a sidecar uses the same virtual-dataset walk.
`list` of either is still one level.

### Branch aggregation and trajectory counts

Branch nodes MAY omit `[stats]` and MUST NOT be treated as trajectory sources.
Only leaf nodes contribute trajectories.

When presenting Dataset previews after `open`, or a leaf card in `list`:

- A leaf's trajectory count is `[stats].record_count` (and `failed_count`)
  when the sidecar is present and the fingerprint is trusted.
- `list` of a Directory MUST NOT roll up descendant counts (that would be a
  deep scan). Roll-up is a Snapshot / `open` concern.
- Intermediate branch directories contribute **0** of their own as sources.
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

Warehouse / Catalog MAY keep an in-process cache of `list` children and
trusted leaf stats so periodic UI refresh does not re-open Lance. Cache
entries SHOULD invalidate when a leaf `fingerprint` or manifest mtime
changes, or when a new `chronicle.manifest` appears among immediate
children. Process cache MUST NOT replace on-disk leaf manifests as the
source of truth that travels with the dataset. `list` refresh MUST NOT run
acceleration SQL or `steps` token/duration queries.

## TOML schema (v1)

### Required top-level fields

| Field | Type | Rules |
|---|---|---|
| `schema_version` | integer | MUST be `1` for this RFC |
| `kind` | string | MUST be `"leaf"` or `"branch"` |

### Leaf-only fields

| Field | Type | Rules |
|---|---|---|
| `format` | string | MUST be present for `kind = "leaf"`; v1 writers MUST use `compact-jsonl/v1` or `storyline/v1` |

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
  `[stats]` for `ls` / Datasets previews without scanning rows.
- When the file is missing, unreadable, or fingerprint mismatches, store-layer
  `ensure_manifest` SHOULD rewrite the sidecar in place; if that fails, readers
  MUST fall back to existing discovery / summary paths.
- Manifest stats MUST NOT be the sole authority for query correctness; SQL and
  record listing still read the physical store.

## Warehouse Datasets / `ls` implications

- CLI default `ls` and `/api/explorer/tree` (or its successor) MUST be the same
  `list(path)`: one level, Dataset children annotated from `chronicle.manifest`
  when present.
- `pchronicle ls --sources` MUST print `open(path)` Snapshot members. SQL
  `dataset.sources` remains the in-query equivalent.
- Web Datasets MUST NOT reconstruct folders from `RunSummary`,
  `explorer_weight`, shallow-nav fallback, or an `other` overflow bucket.
- The Runs **Run paths** tree MAY keep grouping runs by `_file_` / import
  path (run-reconstructed tree). That surface is Runs, not Datasets.
- Clicking a Directory navigates (`list` of that prefix). Clicking a leaf
  Dataset does not drill into Lance interiors. **Open in Runs** `open`s the
  current query root (the serve mount) and MAY filter `_file_`.
- Detailed run/record pages MAY still open the physical leaf; this RFC does
  not require a full sidecar index of every record identity.

## Required unit tests (v1)

Implementations MUST cover at least:

1. **`list` is one level**: `warehouse/(Directory)` with `team/(Directory)` and
   `archive/(leaf, N)` lists `team/` as a directory and `archive` as a dataset
   with `record_count = N`. It MUST NOT list `team/codex_jsonl`.
2. **`open` is a virtual dataset**: `open(warehouse)` yields a compact source at
   `team/codex_jsonl` with `record_count = N` (and any sibling leaves / JSON
   outside leaves), without opening Lance when the leaf manifesto is present.
3. **`list` of a leaf**: `list(archive)` reports one Dataset plus preview and
   MUST NOT list `data/` or `_versions/`.
4. **Leaf non-recursion on `open`**: a leaf directory that also contains a nested
   `chronicle.manifest` MUST NOT emit an additional source for the nested
   child.
5. **Write isolation (contract)**: updating one leaf's manifesto MUST NOT
   require changing parent branch files for that leaf's `list` preview to
   remain correct.
6. **CLI/UI parity**: the Web Datasets children for a prefix match `pchronicle
   ls` of the same path (names, kinds, and Dataset previews).
7. **`ls --sources`**: `ls --sources warehouse` lists `team/codex_jsonl` (and
   sibling Snapshot members) and MUST match `open(warehouse)` / `dataset.sources`.

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
5. **Reconstruct Datasets folders from run summaries / acceleration** —
   rejected for Datasets / default `ls`. The Runs **Run paths** tree MAY
   still group by run `_file_` / import path.
6. **Default `ls` dumping Snapshot members** — rejected. Default `ls` matches
   shell listing. `ls --sources` and `dataset.sources` remain the Snapshot
   member views.
