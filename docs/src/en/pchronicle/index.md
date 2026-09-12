# pChronicle

<img src="/assets/logos/pchronicle-with-text.png" alt="pChronicle logo" width="240" />

**Chronicled Experience for the Agent Era**

*makes every agent run easier to understand and improve*

Agent experience is the sum of everything an agent did. **pChronicle is an Agent
trajectory storage engine**: it records that experience at the unit that matters
— the Run — and makes every Run easier to understand and improve. Use it to
browse, query, exchange, and serve run Datasets produced by Persisting or by
supported external formats; pChronicle does not require pVisor to run.

In Persisting, pChronicle stores and queries trajectory history. It can run as a
local tool or be deployed as a service in front of many paths.

:::tip What you will complete
The first walkthrough creates temporary data, opens it, runs a read-only
summary, and answers one SQL question. You do not need a production store first.
:::

## The one object you work with

A **Dataset** is a path: a local directory or file, or an object-store URI
prefix. pChronicle discovers and normalizes the supported data inside that
path. Dataset pins (`@name`) are locators; after resolution the engine only sees
the path.

A Dataset can be written as:

- a local directory or file (`./local/path`);
- an object-store URI prefix (`s3://bucket/prefix`);
- a dataset pin that resolves to either location (`@name`).

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
pchronicle list ./trajectory-data
pchronicle stats overview ./trajectory-data
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
```

## Choose your next step

Start with [Explore your first Dataset](get-started.md) if you are learning
the command line. It creates temporary data and lets you complete a read-only
query before you connect a real source.

When you already have a question, follow the matching path:

- **Inspect a Dataset:** `pchronicle list DATASET` or `pchronicle stats DATASET`
- **Run a common report:** `pchronicle stats overview DATASET`
- **Ask a custom SQL question:** `pchronicle query DATASET --sql SQL`
- **Name a Dataset:** `pchronicle dataset pin NAME DATASET`
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
