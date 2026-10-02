# System architecture

pChronicle captures, stores, queries, and exchanges Agent trajectory history.
This page defines the boundaries between its engine, interfaces, and supporting
libraries. Physical layouts are documented in [pChronicle Design](../pchronicle/design/index.md).

## Component ownership

| Component | Responsibility |
|---|---|
| `persisting-pchronicle` | Trajectory models, storage, Source discovery, Dataset Snapshots, bounded queries, format conversion, and revision lineage |
| `persisting-pchronicle-cli` | The `pchronicle` command, local read-only Warehouse API, optional Control service, Gateway configuration, and embedded Web assets |
| `pchronicle-web` | Dataset browsing and query UI |
| `persisting-events` | Storage-independent `EventRecord` and optional versioned Control client/protocol |
| `persisting-gateway` | Model protocol adaptation, forwarding, session correlation, and trajectory capture |
| `persisting-overlaynet` | Proxy transport, request classification, and policy for intercepted traffic |
| `persisting-agentctl` | Shared control types, policy transitions, and cooperative client protocol |

## Capture and import

```text
Agent / SDK requests
  → Gateway protocol handling and capture
  → EventRecord
  → pChronicle canonical event storage
  → Dataset Snapshot → bounded queries → CLI / Web / export

Supported files and object-store Sources
  → discovery and pinned Source versions
  → Storyline normalization and query projections
  → Dataset Snapshot → bounded queries → CLI / Web / export
```

Gateway capture records traffic routed through it. Import reads supported
external formats directly. Both paths preserve available identities and source
provenance; neither supplies missing execution or isolation evidence.

## Write and read boundaries

The CLI may call the engine in process. Integrations can also use
`pchronicle serve --control 127.0.0.1:0 DATASET` through the authenticated,
versioned local protocol in `persisting-events`. Only a successful append ACK
confirms durability. A lost response leaves the outcome uncertain, and callers
must not treat it as proof that nothing was written.

pChronicle owns the physical schema, writer fencing, manifest publication, and
maintenance. Producers submit logical records rather than defining parallel
storage layouts. See [RFC-0007](../rfcs/0007-events-contract-pchronicle-sidecar.md)
and [trajectory storage](../pchronicle/design/trajectory-storage.md).

The Warehouse HTTP API and Web UI are read-only. The separately enabled Control
and Gateway capture paths can write. Public bind addresses are rejected by the
local server; the Control protocol is intended for trusted local processes.

## Facts, views, and versions

Canonical events retain their original logical payload. Storyline provides the
normalized model for supported exchange formats; projections support analysis
without replacing the original facts. Queries read a pinned Snapshot, and
revision lineage identifies derived outputs.

Capture coverage is limited to the records a Source contains. Proxy policy
applies to traffic that reaches the proxy and does not establish process-wide
network isolation. See [Gateway capture](../pchronicle/guides/serve-gateway.md)
and [facts and projections](../pchronicle/concepts/facts-and-projections.md).
