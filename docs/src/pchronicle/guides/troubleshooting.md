# Troubleshoot a Dataset

Diagnose a pChronicle result in the same order every time: confirm the path,
inspect what is visible, then narrow the query. This keeps a missing Dataset,
an empty result, and a resource limit from looking like the same failure.

## Confirm the Dataset first

Use a concrete path while investigating. An alias adds one more resolution step:

```bash
pchronicle alias list
pchronicle status ./trajectory-data --format json
pchronicle ls ./trajectory-data --format json
```

If an alias fails, resolve the alias before debugging storage credentials or SQL:

```bash
pchronicle alias get-url prod
pchronicle status @prod --format json
```

An alias points to a Dataset; it does not copy or move the underlying data.

## The Dataset opens but appears empty

Check the summary before writing a more selective query:

```bash
pchronicle analysis overview ./trajectory-data
pchronicle find ./trajectory-data --match "" --format json
```

An empty result can mean that the path contains a supported format with no
matching records, that a filter is scoped to the wrong entity, or that the
Dataset contains files pChronicle does not recognize. The overview and JSON
metadata identify the visible sources and the search mode.

## A query returns no rows

Start with a bounded count, then inspect the normalized table names:

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

Use `find` for identity or text discovery before composing a join. A Snapshot
pins one read view; if data changes between two commands, record the Snapshot
identifier from the JSON output and reuse it in the follow-up query.

## The query stops at a limit

Resource limits are part of the public query contract. Reduce the question
before raising a limit:

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source' \
  --max-output-rows 20 --timeout 10s
```

Use `--file` for a checked-in query and explicit output limits in CI. A query
that needs a larger budget should explain why in the calling workflow rather
than silently removing the guard.

## The source format is unsupported

Check the [supported formats](../reference/formats/index.md) and use the
exchange guide to import into a Dataset pChronicle can normalize. Import does
not invent missing lineage or Evidence; preserve the original Source alongside
the normalized view when provenance matters.

## Before opening an issue

Include the pChronicle version, Dataset path or alias name (without credentials),
the output of `status --format json`, the exact query, and its resource limits.
For object storage, include the provider type and region or endpoint, never
access keys or signed URLs.
