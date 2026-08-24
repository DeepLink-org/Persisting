# pChronicle design

These pages explain the path from a run data source to durable, queryable
history. Storage and API terms used below are defined in the
[terminology guide](../reference/terminology.md).

| Area | Document |
| --- | --- |
| Product boundary and operational guarantees | [Architecture](architecture.md) |
| Dataset discovery, snapshots, and lazy source resolution | [Dataset Catalog](catalog.md) |
| Canonical events and projection ownership | [Run storage](trajectory-storage.md) |
| Three-table Storyline projection and content layer | [Storyline Lance](storyline-lance.md) |

The [pChronicle Reference](../reference/index.md) describes current commands and
formats. Cross-product ownership belongs to [System Design](../../system-design/index.md).
