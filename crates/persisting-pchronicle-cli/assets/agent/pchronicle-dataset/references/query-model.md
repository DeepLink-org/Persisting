# pChronicle query model

Each Dataset is mounted as SQL schema `dataset`. It exposes six stable
relations, although a relation can be empty when no compatible Source
contributes rows:

| Relation | One row represents |
| --- | --- |
| `sources` | a discovered Source candidate |
| `runs` | a normalized Storyline or Session |
| `steps` | a normalized turn |
| `tool_calls` | one tool invocation and linked result |
| `events` | one canonical write-time fact |
| `trajectories` | a Run/Session summary with ordered aggregates |

Use `DESCRIBE dataset.<relation>` for the installed version's exact columns.
Do not assume a column exists across versions or Source formats.

## Identity and joins

IDs are Source-local. A durable address includes the Dataset URI, `_file_`,
entity kind, and original ID. When joining trajectory relations within one
Dataset, join on `_file_` as well as the entity key:

```sql
SELECT r._file_, r.run_id, s.step_id, s.message_kind, s.message_value
FROM dataset.runs r
JOIN dataset.steps s
  ON r._file_ = s._file_
 AND r.session_id = s.session_id
LIMIT 100
```

Use `"$PCHRONICLE_BIN" find` with `--source` when a Source-local ID is
ambiguous.

## Search boundary

Use the unified `find --match` expression before SQL for text and JSONB
lookups. Plain terms and `#field(term)` selectors use the indexed Storyline
Step search path; `$.path=value` is a typed JSONB predicate. There is no
separate JSON search flag. A JSON-only expression searches Runs by default,
while an expression containing text—or an explicit `#json.metrics(...)`
selector—searches Steps.

Treat `find` output as candidate evidence. `search.scope` identifies the
relation, `fts_available` distinguishes an unavailable index from a true empty
result, and `truncated` determines whether the returned count is complete.
Use the returned `_file_`/`source_path`, document, session, and Step identities
to bound any follow-up SQL.

## Read boundary

`pchronicle query` accepts one read-only `SELECT`, `VALUES`, `DESCRIBE`, or
`EXPLAIN` statement. DDL, DML, `COPY`, mutating functions, and multiple
statements are rejected. Prefer aggregates and explicit columns. Retrieve
message, reasoning, argument, result, or payload fields only after narrowing to
specific evidence.

Each command freezes its own Catalog Snapshot. Capture `snapshot_id` from
stderr. If it changes between commands, disclose the change instead of silently
combining the results as one consistent observation.
