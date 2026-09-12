# Discover and query a Dataset

Use this workflow when you have a local path, object-store URI, or dataset pin and
want to understand its run data before writing a report.

!!! tip "What you will have at the end"

    You will know which Sources a Dataset contains, which relations are available,
    and how to answer one bounded, read-only question reproducibly.

## 1. Inspect the Dataset

```bash
pchronicle list ./dataset
pchronicle stats ./dataset
```

`list` (`ls`) lists one directory level, like a shell `ls`. A child directory
with `chronicle.manifest` (or another Dataset marker) is shown as a Dataset
with type and trajectory count. `ls --sources` opens the path and prints the
Snapshot members that `query` will union. `stats` summarizes Dataset readiness after the
path is opened as a Snapshot. Automation can still use
JSON:

```bash
pchronicle list ./dataset --format json
```

If the Dataset may contain malformed entries, choose the error policy:

```bash
pchronicle list ./dataset --errors report
pchronicle list ./dataset --errors strict
```

Use `report` while exploring unfamiliar data. Switch to `strict` in automation
when an incomplete Dataset should fail the job instead of producing a partial
answer.

## 2. Start with a built-in analysis

```bash
pchronicle stats overview ./dataset
pchronicle stats agents ./dataset
pchronicle stats models ./dataset
pchronicle stats tools ./dataset
```

Built-in analysis covers common summaries. Move to SQL when you need custom
filtering, joins, or aggregation.

## 3. Inspect the query schema

```bash
pchronicle query ./dataset --sql "DESCRIBE dataset.steps"
```

Common relations include `sources`, `runs`, `steps`, `tool_calls`, `events`,
and `trajectories`. The relations available depend on the Dataset contents.

## 4. Ask a resource-limited question

```bash
pchronicle query ./dataset \
  --sql "SELECT session_id, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY session_id
         ORDER BY steps DESC"
```

Use `--format jsonl|csv` and `--output` in pipelines. Queries are read-only and
limited by explicit row, byte, discovery, and timeout budgets.

## 5. Locate, then analyze

`list`/`ls` lists the current path. `ls --sources` and `query` **open** that path: a leaf Dataset
is one source; a plain directory becomes a virtual dataset of nested Datasets
and JSON. `find` locates candidates inside a pinned Snapshot. CLI `--match` and Web `q` share the same expression,
reported scope, and `snapshot_id`; the Web UI may highlight returned fields
without changing the match set.

```bash
pchronicle find ./dataset --match "timeout" --format json
```

Use the returned `source_path`, session, and step identities to narrow SQL.

## 6. Disambiguate repeated external IDs

The same external ID may occur in more than one file. Locate candidates first,
then retain `source_path` when you need a durable reference:

```bash
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --source nested/source.json \
  --session-id session-42
```

For exact flags, see the [`pchronicle` CLI reference](../reference/cli.md).
For table fields and join rules, see the [query model](../reference/query-model.md).
Internal discovery and versioning behavior belongs to
[Snapshot design](../design/catalog.md).
