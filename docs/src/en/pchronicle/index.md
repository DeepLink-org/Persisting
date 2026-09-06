# pChronicle

<img src="/img/logos/pchronicle-with-text.png" alt="pChronicle logo" width="240" />

**pChronicle is an Agent trajectory storage engine.** Use it to browse, query,
exchange, and serve run Datasets produced by Persisting or by supported
external formats; pChronicle does not require pVisor to run.

Within Persisting's model-state-to-Agent-history story, pChronicle is the
durable store and query engine for Agent history. It can run as a local tool
or be deployed as a platform in front of many paths.

:::tip What you will complete
The first walkthrough creates temporary data, opens it as a Dataset, runs a
read-only summary, and answers one SQL question. You can learn the query model
without preparing a production store first.
:::

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

## Choose your next step

Start with [Explore your first Dataset](get-started.md) if you are learning
the command line. It creates temporary data and lets you complete a read-only
query before you connect a real source.

When you already have a question, follow the matching path:

- **Inspect a Dataset:** `pchronicle ls DATASET` or `pchronicle status DATASET`
- **Run a common report:** `pchronicle analysis overview DATASET`
- **Ask a custom SQL question:** `pchronicle query DATASET --sql SQL`
- **Name a Dataset:** `pchronicle alias add NAME DATASET`
- **Import or export runs:** [Exchange data](guides/exchange.md)
- **Analyze with an Agent:** `pchronicle agent codex DATASET`
- **Open the local UI and API:** [Serve a Dataset](guides/ui.md)

pChronicle reads and organizes run history. It does not execute or schedule
Agents. To run an Agent in a controlled workspace, start with
[pVisor](../pvisor/index.md).

## A useful reading order

1. [Explore your first Dataset](get-started.md) to complete a read-only query.
2. [Discover and query](guides/discover-and-query.md) when you are ready to inspect real data.
3. [Exchange data](guides/exchange.md) when a supported external format is involved.
4. [Serve a Dataset](guides/serve.md) when another tool needs local access.
5. [CLI reference](reference/cli.md) when you need exact flags, budgets, or output formats.

## Keep reading

- [Explore your first Dataset](get-started.md)
- [Follow common workflows](guides/index.md)
- [Look up the complete CLI](reference/cli.md)
- [Use the shared product terminology](reference/terminology.md)
- [Understand the data model](concepts/index.md)
- [Inspect storage and Snapshot design](design/index.md)
