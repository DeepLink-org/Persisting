# Explore a Run Dataset

pChronicle gives you one interface for Agent runs stored locally, in
object storage, or behind a configured alias. The inspect, find, analysis, and
query commands in this walkthrough are read-only.

## 1. Try pChronicle without preparing data

```bash
pchronicle onboard
```

The walkthrough creates a temporary example Dataset and introduces the main
commands. To jump directly to querying:

```bash
pchronicle onboard query
```

Neither command requires a source checkout or an existing Dataset.

## 2. Inspect a Dataset you already have

The onboarding Dataset is temporary and is removed when the walkthrough ends.
For a persistent query, point the commands below at a Dataset path you already
own. If you do not have one yet, stop after the onboarding query and continue
with [Discover and query your own data](guides/discover-and-query.md).

A Dataset may be a local path, an object-store URI prefix, or an alias such as
`@prod`:

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
```

`ls` shows the run data pChronicle can use. `analysis overview` gives a
stable summary without requiring SQL.

To locate content, use the unified `find --match` syntax:

```bash
pchronicle find ./trajectory-data --match "timeout" --format json
pchronicle find ./trajectory-data --match '#system("retry")'
```

## 3. Ask a specific question

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY source
         ORDER BY source'
```

Queries are read-only and limited by explicit resource budgets. pChronicle
normalizes supported run data formats into common `runs`, `steps`, and `tool_calls` tables where their
semantics align.

## What you completed

You opened one Dataset, ran a built-in summary, queried a normalized table, and
can now locate specific trajectories with FTS/JSONB. The Dataset was not modified.

Continue by task:

- [Discover and query your own Dataset](guides/discover-and-query.md)
- [Import or export runs](guides/exchange.md)
- [Review the product terminology](reference/terminology.md)
- [Use aliases and the complete CLI](reference/cli.md)
- [Capture a new Run with pVisor](../pvisor/guides/capture.md)
- [Learn the pChronicle concepts](concepts/index.md)
