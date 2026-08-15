# Explore durable Run history

`pChronicle` reads trajectory Datasets from local directories or S3, discovers
supported source formats, and exposes normalized `runs`, `steps`, and
`tool_calls` tables.

It is the history layer after execution—not the runtime or scheduler.

## Start with a known Dataset

From a Persisting source checkout:

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
```

`ls` shows the discovered Sources. The overview reports the shape of the
Dataset without requiring you to write SQL first.

## Ask a specific question

```bash
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps
   FROM dataset.steps
   GROUP BY source
   ORDER BY source'
```

Queries are read-only. Format-specific data is normalized at the Dataset
boundary so the same question can span ATIF, ACTF, OpenAI Messages, canonical
events, and Storyline Sources where their semantics align.

## What you completed

You discovered the logical Sources in one Dataset, created a Catalog Snapshot,
ran a stable built-in summary, and queried a normalized relation. No Dataset
content was modified.

Continue by task:

- [Discover and query your own Dataset](guides/discover-and-query.md).
- [Capture a new Run with pVisor](../pvisor/guides/capture.md).
- [Import or export trajectories](guides/exchange.md).
- [Understand Dataset identity and Snapshots](concepts/dataset-and-source.md).
- Look up exact flags in the [`pchronicle` reference](reference/cli.md).
