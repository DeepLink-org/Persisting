# pChronicle

**pChronicle is Persisting's structured trajectory and Dataset data layer.** It
discovers native and supported external Sources on local storage or S3,
preserves canonical event facts where present and keeps Source origin visible,
exposes normalized Run views, and supports bounded query, analysis, revision
lineage, and format exchange.

![pChronicle product boundary](../assets/diagrams/persisting/pchronicle-product.svg)

## What pChronicle owns

- Dataset and Source discovery;
- immutable Catalog Snapshot membership and source version descriptions;
- canonical event storage and terminal Run facts;
- normalized `runs`, `steps`, and `tool_calls` query views;
- import boundaries for ATIF, ACTF, and OpenAI Messages;
- export boundaries for those formats and Storyline JSON;
- AgenticMD as a non-authoritative human-readable projection;
- revision lineage for derived data.

pChronicle does not execute or schedule Agents. Its inputs include canonical
runtime-event Sources and pinned local or S3 ATIF, ACTF, OpenAI Messages, and
Storyline Sources. External Sources are normalized directly; they do not first
become canonical runtime events.

## Ask the first question

```bash
pchronicle onboard
pchronicle onboard query
```

These installed-product walkthroughs create temporary example Datasets and do
not require a source checkout.

## Read pChronicle by purpose

| Goal | Section |
| --- | --- |
| Query the first Dataset | [Get Started](get-started.md) |
| Understand Dataset, Source, events, and projections | [Concepts](concepts/index.md) |
| Follow common trajectory data workflows | [Guides](guides/index.md) |
| Inspect storage and catalog mechanisms | [Design](design/index.md) |
| Look up commands, schemas, and formats | [Reference](reference/index.md) |

For Persisting-governed capture through pVisor's configured Gateway and
lifecycle-event path, begin with [pVisor](../pvisor/index.md).
