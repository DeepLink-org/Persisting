# Query model reference

Every mounted Dataset is a SQL schema. A positional Dataset is named `dataset`;
`--mount NAME=DATASET` creates a named schema. The schema exposes six stable
relations even when no compatible Source contributes rows.

| Relation | One row represents | Available from |
| --- | --- | --- |
| `sources` | one discovered Source candidate | every Dataset |
| `runs` | one normalized Storyline/session | every ready trajectory Source |
| `steps` | one normalized turn | every ready trajectory Source |
| `tool_calls` | one tool invocation and its linked result | every ready trajectory Source |
| `events` | one canonical write-time fact | canonical event Sources only |
| `trajectories` | one Run/session summary with ordered Step and tool aggregates | normalized trajectory Sources |

Use `DESCRIBE` for the exact columns exposed by the installed version:

```sql
DESCRIBE dataset.sources;
DESCRIBE dataset.runs;
DESCRIBE dataset.steps;
DESCRIBE dataset.tool_calls;
DESCRIBE dataset.events;
DESCRIBE dataset.trajectories;
```

## Source identity

Entity IDs are Source-local. `runs`, `steps`, `tool_calls`, and `events` retain
`_file_`, the Dataset-relative `source_path`. A durable entity address includes
the Dataset URI, `_file_`, entity kind, and original ID.

When joining built-in trajectory relations inside one Dataset, include `_file_`
alongside the entity key:

```sql
SELECT r.run_id, s.step_id, s.message_json
FROM dataset.runs r
JOIN dataset.steps s
  ON r._file_ = s._file_
 AND r.session_id = s.session_id;
```

A built-in join that omits `_file_` is rejected because equal IDs in two
Sources do not identify the same entity. Across separately named Datasets,
`_file_` equality is not required because each schema is already a different
namespace.

## `sources`

| Column | Type | Meaning |
| --- | --- | --- |
| `_file_` | UTF-8, non-null | Dataset-relative Source path |
| `format` | UTF-8, nullable | detected or declared representation |
| `kind` | UTF-8, non-null | `store` or `file` |
| `snapshot_ref` | UTF-8, nullable | generation, manifest revision, fingerprint, version, or ETag |
| `size_bytes` | UInt64, nullable | candidate file or marker-object size |
| `last_modified` | UTF-8, nullable | RFC 3339 timestamp when available |
| `projection_status` | UTF-8, nullable | `fresh` or `stale` for a canonical events Source with a linked Storyline projection |
| `projection_generation` | UTF-8, nullable | generation selected as the read acceleration projection |
| `projection_candidates` | UInt64, non-null | number of linked projection candidates considered |
| `status` | UTF-8, non-null | `ready` or `error` |
| `error` | UTF-8, nullable | sanitized discovery or resolution error |

`format` may remain null until a selected peripheral file is opened lazily.
Filtering `_file_` can prevent unrelated Sources from being opened.
`snapshot_ref` is a display projection; Rust/API consumers use the typed
`CatalogSourceRevision` for consistency decisions.

## Query boundary

The engine accepts one read-only `SELECT`, `VALUES`, `DESCRIBE`, or `EXPLAIN`
statement. It rejects DDL, DML, `COPY`, mutating functions, and multiple
statements. CLI row, byte, discovery, and timeout limits still apply.

Exact physical Storyline columns are documented by
[Storyline Lance](../design/storyline-lance.md). Discovery and predicate-pruning
mechanisms belong to [Dataset Catalog design](../design/catalog.md). Use the
[query guide](../guides/discover-and-query.md) for a complete workflow.
