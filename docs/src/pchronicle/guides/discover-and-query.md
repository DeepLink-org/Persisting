# Discover and query a Dataset

Use this workflow when you have a local directory or S3 prefix and need to
understand its contents before writing analysis.

## 1. Discover logical Sources

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
```

`ls` reports logical Sources. `status` summarizes the immutable Catalog
Snapshot selected for the command. Use JSON output in automation:

```bash
pchronicle ls ./dataset --format json
```

If a Dataset may contain malformed Sources, choose the error policy explicitly:

```bash
pchronicle ls ./dataset --errors report
pchronicle ls ./dataset --errors strict
```

## 2. Start with a stable analysis

```bash
pchronicle analysis overview ./dataset
pchronicle analysis agents ./dataset
pchronicle analysis models ./dataset
pchronicle analysis tools ./dataset
```

Built-in analysis is useful for common summaries. Move to SQL when the question
needs a custom projection.

## 3. Inspect the logical schema

Do not assume that physical exchange fields are SQL columns:

```bash
pchronicle query ./dataset "DESCRIBE dataset.steps"
```

Common logical relations include `sources`, `runs`, `steps`, `tool_calls`,
`events`, and `trajectories`. Availability is reported per Source.

## 4. Ask a bounded question

```bash
pchronicle query ./dataset \
  "SELECT session_id, COUNT(*) AS steps
   FROM dataset.steps
   GROUP BY session_id
   ORDER BY steps DESC"
```

Use `--format jsonl|csv` and `--output` for pipelines. Queries are read-only and
bounded by row, byte, discovery, and timeout limits.

## 5. Disambiguate external IDs

IDs are Source-local. Locate candidates first, then retain `source_path` in any
durable reference:

```bash
pchronicle find ./dataset --session-id session-42
pchronicle find ./dataset --source nested/source.json \
  --session-id session-42
```

For exact flags, see the [`pchronicle` reference](../reference/cli.md). For why
Sources and Snapshots behave this way, read
[Dataset, Source, and Snapshot](../concepts/dataset-and-source.md).
