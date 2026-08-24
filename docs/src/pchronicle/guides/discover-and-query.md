# Discover and query a Dataset

Use this workflow when you have a local path, object-store URI, or alias and
want to understand its run data before writing a report.

## 1. Inspect the Dataset

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
```

`ls` shows the independently queryable run data sources pChronicle found.
`status` summarizes Dataset readiness and available data. Use JSON in
automation:

```bash
pchronicle ls ./dataset --format json
```

If the Dataset may contain malformed entries, choose the error policy:

```bash
pchronicle ls ./dataset --errors report
pchronicle ls ./dataset --errors strict
```

## 2. Start with a built-in analysis

```bash
pchronicle analysis overview ./dataset
pchronicle analysis agents ./dataset
pchronicle analysis models ./dataset
pchronicle analysis tools ./dataset
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

## 5. Disambiguate repeated external IDs

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
[Dataset Catalog design](../design/catalog.md).
