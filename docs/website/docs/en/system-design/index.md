# System Design

Persisting is persistent infrastructure for the Agent era, spanning model
state—parameters and KV caches—and Agent history. This section focuses on the
current public product path:

- [pVisor](../pvisor/index.md) virtualizes and governs one Agent Run;
- [pChronicle](../pchronicle/index.md) organizes durable trajectory Sources into
  queryable Datasets.

Gateway, OverlayFS, and OverlayNet are pVisor runtime mechanisms. Where
available, stable Run identity connects the domains, but each also has a
standalone entry path.

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## Cross-product contract

```text
Configured pVisor capture
  Gateway trajectory events ─┐
  pVisor lifecycle records ──┴─> canonical event Source ─┐
Pinned external Sources                                  │
  ATIF / ACTF / OpenAI Messages / Storyline ─────────────┴─> Snapshot
                                                               └─> normalized Dataset views
```

Attempt finalization writes a private, versioned Run Bundle and leaves Effects
staged for later review/apply/drop without pChronicle. Configured capture sends
Gateway trajectory events and pVisor lifecycle records, including the Evidence
those records carry. The full Bundle and its Artifact, lineage, Effect, and
broader Evidence inventory remain local unless moved separately.

External file and Storyline Sources are pinned and normalized directly without
passing through pVisor or becoming canonical events. Each path retains
source-specific guarantees; ingestion does not add Evidence that its Source did
not provide.

| Concern | Owner |
| --- | --- |
| One Run's execution boundary | pVisor |
| Model, network, and filesystem runtime drivers | pVisor |
| Canonical events, terminal facts, and Dataset history | pChronicle |
| Query, exchange, and revision lineage | pChronicle |

## Continue by question

- [Complete architecture and target model](architecture.md)
- [Local-to-fleet continuity](local-to-fleet.md)
- [Security and evidence model](security-evidence.md)
- [pVisor implementation boundaries](../pvisor/design/index.md)
- [pChronicle implementation boundaries](../pchronicle/design/index.md)

Delivery state is reported in the product Design pages and
[Project Engineering Notes](../project/engineering.md). Target architecture is
not evidence that a capability is implemented.
