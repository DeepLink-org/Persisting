# pChronicle

**pChronicle is Persisting's structured trajectory and Dataset data layer.** It
discovers native and supported external Sources on local storage or S3,
preserves canonical facts and origin, exposes normalized Run views, and
supports bounded query, analysis, revision lineage, and format exchange.

![pChronicle product boundary](../assets/diagrams/persisting/pchronicle-product.svg)

## What pChronicle owns

- Dataset and Source discovery;
- immutable Catalog Snapshot membership and source version descriptions;
- canonical event storage and terminal Run facts;
- normalized `runs`, `steps`, and `tool_calls` query views;
- Storyline, ATIF, ACTF, and OpenAI Messages interchange boundaries;
- AgenticMD as a non-authoritative human-readable projection;
- revision lineage for derived data.

pChronicle does not execute or schedule Agents. It begins where runtime events
become durable history.

## Ask the first question

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

## Read pChronicle by purpose

| Goal | Section |
| --- | --- |
| Query the first Dataset | [Get Started](get-started.md) |
| Understand Dataset, Source, events, and projections | [Concepts](concepts/index.md) |
| Follow common trajectory data workflows | [Guides](guides/index.md) |
| Inspect storage and catalog mechanisms | [Design](design/index.md) |
| Look up commands, schemas, and formats | [Reference](reference/index.md) |

To understand how the Runs are executed and captured, begin with
[pVisor](../pvisor/index.md).
