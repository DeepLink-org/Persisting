# RFC-0003: pChronicle ownership

pChronicle owns Storyline models, peripheral format codecs, physical storage,
Source discovery, Snapshot construction, DataFusion queries, and revision
lineage. CLI and Web consume those contracts. They must not introduce a second
trajectory model or a parallel persistence format.

The CLI owns commands, import/export orchestration, the loopback Warehouse
HTTP server, and static asset embedding. The Web owns the browsing interface.

[Architecture](../pchronicle/design/architecture.md)
