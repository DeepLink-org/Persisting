# pChronicle

**pChronicle lets you browse, query, exchange, and serve Agent trajectory
Datasets.** Use it with trajectories produced by Persisting or with supported
external formats; pChronicle does not require pVisor to run.

Within Persisting's model-state-to-Agent-history story, pChronicle is the
durable, queryable Agent-history layer.

## The one object you work with

A **Dataset** is the single object operated on by pChronicle. It is a collection
of Agent trajectory data that can be inspected, queried, analyzed, imported,
exported, or served.

A Dataset can be:

- a local directory or file (`./local/path`);
- an object-store URI prefix (`s3://bucket/prefix`);
- a user alias that points to either location (`@alias-name`).

pChronicle discovers and normalizes the supported data inside that location.
You do not need to understand its internal files or storage layout before using
the CLI.

## Start here

Try the built-in walkthrough without preparing any data:

```bash
pchronicle onboard
```

Or inspect and query an existing Dataset:

```bash
pchronicle ls ./trajectory-data
pchronicle analysis overview ./trajectory-data
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

## Choose a task

| I want to... | Start with |
| --- | --- |
| Inspect a Dataset | `pchronicle ls DATASET` or `pchronicle status DATASET` |
| Run a common report | `pchronicle analysis overview DATASET` |
| Ask a custom SQL question | `pchronicle query DATASET --sql SQL` |
| Give a Dataset a short name | `pchronicle alias add NAME DATASET` |
| Import or export trajectories | `pchronicle import` or `pchronicle export` |
| Analyze with Codex or Claude | `pchronicle agent codex DATASET` |
| Open the local read-only UI and API | `pchronicle serve DATASET` |

pChronicle reads and organizes trajectory data. It does not execute or schedule
Agents. To run an Agent in a controlled workspace, start with
[pVisor](../pvisor/index.md).

## Keep reading

- [Explore your first Dataset](get-started.md)
- [Follow common workflows](guides/index.md)
- [Look up the complete CLI](reference/cli.md)
- [Understand the data model](concepts/index.md)
- [Inspect storage and catalog design](design/index.md)
