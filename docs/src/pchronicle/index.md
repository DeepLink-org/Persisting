# pChronicle

**pChronicle is an Agent trajectory storage engine.** Use it to browse, query,
exchange, and serve run Datasets produced by Persisting or by supported
external formats; pChronicle does not require pVisor to run.

Within Persisting's model-state-to-Agent-history story, pChronicle is the
durable store and query engine for Agent history. It can run as a local tool
or be deployed as a platform in front of many paths.

## The one object you work with

A **Dataset** is a path: a local directory or file, or an object-store URI
prefix. pChronicle discovers and normalizes the supported data inside that
path. Aliases (`@name`) are locators; after resolution the engine only sees
the path.

A Dataset can be written as:

- a local directory or file (`./local/path`);
- an object-store URI prefix (`s3://bucket/prefix`);
- a user alias that resolves to either location (`@alias-name`).

pChronicle discovers and normalizes the supported data inside that path.
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
| Import or export runs | `pchronicle import` or `pchronicle export` |
| Analyze with Codex or Claude | `pchronicle agent codex DATASET` |
| Open the local read-only UI and API | [`pchronicle serve DATASET`](guides/ui.md) |

pChronicle reads and organizes run history. It does not execute or schedule
Agents. To run an Agent in a controlled workspace, start with
[pVisor](../pvisor/index.md).

## Keep reading

- [Explore your first Dataset](get-started.md)
- [Follow common workflows](guides/index.md)
- [Look up the complete CLI](reference/cli.md)
- [Use the shared product terminology](reference/terminology.md)
- [Understand the data model](concepts/index.md)
- [Inspect storage and Snapshot design](design/index.md)
