# pChronicle architecture

This document explains how pChronicle turns trajectory Sources into durable,
queryable history. User workflows belong to [Guides](../guides/index.md), exact
commands to [Reference](../reference/cli.md), and cross-product ownership to
[System Design](../../system-design/architecture.md).

![pChronicle product boundary](../../assets/diagrams/persisting/pchronicle-product.svg)

## Product boundary

pChronicle is a path-first Agent history layer. It discovers local directories
and object-store prefixes, fixes the Source versions used by an operation,
normalizes supported representations, and exposes bounded read surfaces.

It has four deployment shapes:

| Shape | Purpose | Persistent state |
| --- | --- | --- |
| direct Dataset | inspect a local path or S3 prefix | none outside the Dataset |
| native Dataset | receive canonical events or create-only import | Dataset manifests and versions |
| default local Dataset | omit the Dataset argument for one local root | normalized path in user configuration |
| read-only Warehouse | mount static Datasets for Web and API review | configuration and rebuildable cache |

pChronicle is not a scheduler, Agent runtime, global Dataset control plane,
distributed SQL service, or time-series database. The loopback server has no
authentication and is not a production multi-tenant endpoint.

## Data layers and ownership

```text
writers and importers
  → canonical events and terminal facts
  → logical Run / Step / ToolCall projections
  → exchange representations
  → lineage-bearing revisions
```

| Layer | Ownership rule |
| --- | --- |
| canonical events | write-time facts; append-oriented source of truth |
| logical projection | normalized, rebuildable query view |
| exchange representation | import/export contract; never silently promoted to global truth |
| revision | derived output with parent Snapshot and transform lineage |

Storyline is a session-oriented projection. Its Lance layout is optimized for
reconstructing complete documents, so replacement of one session is not the
canonical high-rate append path.

## Dataset addressing

A normalized Dataset URI is the resource identity. A Source path names one
independently discovered and versioned representation inside it. External IDs
remain Source-local:

```text
(dataset_uri, source_path, entity_kind, original_id)
```

Warehouse mount names are SQL aliases only. Moving data to another URI creates
a different Dataset identity. Credentials must not be embedded in Dataset URIs.

## Read path

```text
Dataset URI or static mount
  → bounded discovery
  → immutable Catalog Snapshot
  → Source pruning and lazy open
  → normalized DataFusion relations
  → bounded CLI, API, or Web result
```

One operation fixes each Source's version reference. Local files use identity
and fingerprint checks; Lance stores use their published generation or manifest;
object stores use a version or conditional ETag where available. The Snapshot
does not claim that unrelated Sources share a global transaction time.

Normalized relations include `sources`, `runs`, `steps`, `tool_calls`, `events`,
and `trajectories` when supported by a Source. Every entity relation retains
`source_path`. SQL is read-only and rejects DDL, DML, network functions, and file
functions.

The detailed discovery and pruning algorithm belongs to
[Dataset Catalog design](catalog.md).

## Write and publication path

Gateway and native writers publish canonical events before derived views.
Objects referenced by a Snapshot must be durable before the publication pointer
becomes visible. A failed publication leaves the previous Snapshot readable.

```text
validate event
  → persist payload or content-addressed object
  → append canonical fact
  → publish terminal fact or projection generation
  → expose through a later Catalog Snapshot
```

Writer concurrency is defined by the concrete store contract. Snapshot compare-
and-swap alone does not imply merge-and-retry behavior. Unpublished versions and
unreachable objects require an explicit maintenance path.

The canonical/projection boundary is detailed in
[Trajectory storage](trajectory-storage.md); the Storyline implementation is
documented in [Storyline Lance](storyline-lance.md).

## Read-only Warehouse

The server mounts a static set of named Datasets. Refresh builds a complete new
Catalog Snapshot before switching readers. Dataset tables prune by Source before
opening matching fixed versions; caches and routing indexes are tied to that
Snapshot generation.

The Web application and API are consumers of the same read model. They do not
become another source of truth. Unknown API routes remain errors rather than SPA
fallbacks, and only loopback listeners are accepted while authentication is
absent.

User setup belongs to the [Warehouse guide](../guides/serve.md). Exact routes and
Gateway composition belong to the [`pchronicle` reference](../reference/cli.md).

## Guarantees and explicit non-guarantees

| Area | Guarantee | Non-guarantee |
| --- | --- | --- |
| identity | Dataset URI + Source path + original ID remains visible | global uniqueness of external IDs |
| read consistency | fixed Source references within one operation | global transaction across Sources |
| publication | previous Snapshot remains readable until new publication succeeds | automatic merge retry for every writer |
| query | bounded, read-only execution | arbitrary mutation or unbounded service query |
| projection | lineage and rebuildability where declared | projection freshness without a recorded generation |
| service | loopback-only static read surface | authenticated multi-tenant Warehouse |

## Related design documents

- [Dataset Catalog](catalog.md): discovery, Snapshot construction, lazy Source
  resolution, and pruning.
- [Trajectory storage](trajectory-storage.md): canonical facts, physical
  representations, and write ownership.
- [Storyline Lance](storyline-lance.md): three-table projection, content layer,
  publication, and maintenance.
- [Facts, projections, and revisions](../concepts/facts-and-projections.md): the
  user-facing mental model behind these layers.
- [pChronicle Reference](../reference/index.md): exact CLI and format contracts.
