# System design

Persisting centers on [pChronicle](../pchronicle/index.md), an Agent trajectory
storage engine. Gateway captures model traffic, shared event contracts carry
records into storage, and the CLI and Web UI expose Datasets for inspection.

```text
Gateway capture ── EventRecord ── pChronicle storage ─┐
ATIF / ACTF / OpenAI Messages / Storyline Sources ────┴─> Dataset Snapshot
                                                           └─> query / exchange / Web UI
```

Canonical events preserve recorded facts. Storyline normalization and query
projections make supported Sources usable through common views. A Snapshot pins
the Source versions used to answer a query; importing data does not add facts
that the Source did not provide.

## Continue by question

- [Component ownership and data flow](architecture.md)
- [Design principles](design-principles.md)
- [Storage and query implementation](../pchronicle/design/index.md)
- [Gateway capture](../pchronicle/guides/serve-gateway.md)
- [Project engineering notes](../project/engineering.md)
