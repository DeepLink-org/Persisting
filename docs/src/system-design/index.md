# System Design

Persisting has two primary product domains:

- [pVisor](../pvisor/index.md) virtualizes and governs Agent execution;
- [pChronicle](../pchronicle/index.md) organizes durable trajectory Sources into
  queryable Datasets.

pPilot extends pVisor from one Run to many. Gateway, OverlayFS, and OverlayNet
are pVisor runtime mechanisms. The product domains integrate through stable Run
identity, events, artifacts, terminal facts, lineage, and Evidence, but each has
a standalone entry path.

![Persisting product domains and integration](../assets/diagrams/persisting/system-products.svg)

## Cross-product contract

```text
Agent goal -> pVisor / pPilot -> events + artifacts + terminal facts + Evidence
                                                   |
External Sources -> importer / adapter ------------+-> pChronicle Dataset
```

A pVisor Run can end with reviewable staged Effects and a private, versioned Run
Bundle without pChronicle. The standard durable handoff sends its observed
facts and Evidence to pChronicle. External Sources can enter the same Dataset
model through a supported importer or adapter without passing through pVisor.
Each path retains source-specific guarantees; ingestion does not add evidence
that its source did not provide.

| Concern | Owner |
| --- | --- |
| One Run's execution boundary | pVisor |
| Planning and recovery of many Runs | pPilot |
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
