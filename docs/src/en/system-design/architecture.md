# System architecture

The pChronicle library owns trajectory models, format conversion, discovery,
Lance storage, snapshots, and bounded queries. The CLI owns import/export
orchestration and the loopback Warehouse API. The Web UI consumes those
models and API contracts.

Storyline documents are written directly. Published table versions and writer
leases preserve storage consistency; queries read fixed source versions.
Dataset, Source, and revision identifiers keep transformed data traceable.

[pChronicle architecture](../pchronicle/design/architecture.md)
