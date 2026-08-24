# Explore a trajectory Dataset

pChronicle gives you one interface for Agent trajectories stored locally, in
object storage, or behind a configured alias. Commands are read-only unless you
explicitly run `import` or `export` with a destination.

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

## 2. Inspect your own Dataset

A Dataset may be a local path, an object-store URI prefix, or an alias such as
`@prod`:

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
```

`ls` shows the trajectory data pChronicle can use. `analysis overview` gives a
stable summary without requiring SQL.

## 3. Ask a specific question

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps
         FROM dataset.steps
         GROUP BY source
         ORDER BY source'
```

Queries are bounded and read-only. pChronicle normalizes supported trajectory
formats into common `runs`, `steps`, and `tool_calls` tables where their
semantics align.

## What you completed

You opened one Dataset, ran a built-in summary, and queried a normalized table.
The Dataset was not modified.

Continue by task:

- [Discover and query your own Dataset](guides/discover-and-query.md)
- [Import or export trajectories](guides/exchange.md)
- [Use aliases and the complete CLI](reference/cli.md)
- [Capture a new Run with pVisor](../pvisor/guides/capture.md)
- [Understand the Dataset interface](concepts/index.md)
