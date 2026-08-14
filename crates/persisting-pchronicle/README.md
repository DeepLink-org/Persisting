# pChronicle

**Persisting's structured trajectory and Dataset core.**

pChronicle owns trajectory formats, physical schemas, persistence, discovery,
query execution, exchange, and derived views. Other crates may produce or
consume trajectories but must not define a second storage format.

The core crate has no HTTP or Web dependency. Product boundaries are split as:

| Component | Responsibility |
|---|---|
| `persisting-pchronicle` | formats, catalog snapshots, Lance storage, readers, query engine |
| `persisting-pchronicle-cli` | the standalone `pchronicle` command |
| `persisting-pchronicle-server` | loopback-only read API and embedded Web assets |
| `pchronicle-web` | Dioxus browser frontend |

```bash
pchronicle ls ./dataset
pchronicle status ./dataset
pchronicle query ./dataset "SELECT * FROM dataset.runs"
pchronicle analysis overview ./dataset
pchronicle import --from input.json --output ./imported --format atif
pchronicle export --from ./imported --output output.json --format storyline
pchronicle serve --config warehouse.toml
```

Canonical capture uses append-only `events.lance`. Normalized analytical data
uses one atomically published Storyline generation containing `runs.lance`,
`steps.lance`, and `tool_calls.lance`; large content may be addressed through
`objects.lance`. A `DatasetCatalogSnapshot` freezes discovered local or S3
Sources for each operation and exposes normalized SQL relations.

See the [`pchronicle` command reference](../../docs/src/design/cli-pchronicle.md)
and [RFC-0003](../../docs/src/rfcs/0003-pchronicle-ownership.md).
