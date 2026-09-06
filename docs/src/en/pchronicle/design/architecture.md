# pChronicle architecture

This document explains how pChronicle stores Agent trajectories and exposes
resource-limited read surfaces. User workflows belong to [Guides](../guides/index.md),
exact commands to [Reference](../reference/cli.md), and cross-product ownership to
[System Design](../../system-design/architecture.md).

![pChronicle product boundary](../../assets/diagrams/persisting/pchronicle-product.svg)

## Product boundary

pChronicle is an Agent trajectory **storage engine**. It can be used as a local
tool or deployed as a platform in front of many paths. CLI, Web, Agent, and
Warehouse are clients of the engine, not separate products.

The engine API is:

```text
open(path) → pin Snapshot → discover / locate / analyze (and append on write paths)
```

A **Dataset is a path**: a normalized local path or object-store URI
(`s3://`, `az://`, `gs://`). Mount names, `@alias`, and Directory library names
are locators. After resolution the engine only sees a path. Credentials must not
be embedded in that path.

It has four deployment shapes:

| Shape | Purpose | Persistent state |
| --- | --- | --- |
| direct Dataset | inspect a local path or S3 prefix | none outside the Dataset |
| native Dataset | receive canonical events or create/append/replace import | Dataset manifests and versions |
| default local Dataset | omit the Dataset argument for one local root | normalized path in user configuration |
| read-only Warehouse | mount static paths for Web and API review; optional Directory for path ACL | configuration and rebuildable cache |

pChronicle is not a scheduler, Agent runtime, global Dataset control plane,
distributed SQL service, or time-series database. The loopback Warehouse does
not accept non-loopback binds. Without `--catalog-config` it has no user
authentication. With `--catalog-config`, Directory and data-plane routes require
user access/secret headers; this is still not a public multi-tenant service.
See [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).

## Four layers

| Layer | Role | Not |
| --- | --- | --- |
| **Path** | Dataset identity. Local path or object-store URI. | A mount name, `@alias`, library name, or `catalog://` URI |
| **Directory** (optional) | Platform addressing: resolve a name to a path and decide who may open it. After a ticket, the client opens the path. | A third Dataset kind. Not a Snapshot. |
| **Snapshot** | Sync protocol between writers and readers on a path: which Sources exist, which version each is pinned to. | A product named Catalog. Not the Directory listing. |
| **Query surface** | Discover (`ls` / `sources`), locate (`find`), analyze (`query`). All relative to a pinned Snapshot. | A fourth semantics in the Web Explorer |

Code may still use names such as `DatasetCatalogSnapshot` and `--catalog-config`.
User-facing and RFC language uses Path, Directory, and Snapshot.

The Directory is specified by [RFC-0013](../../rfcs/0013-pchronicle-warehouse-catalog.md).
Snapshot construction is specified by the [Snapshot design](catalog.md).

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

A normalized path is the resource identity. A Source path names one independently
discovered and versioned representation inside it. External IDs remain
Source-local:

```text
(path, source_path, entity_kind, original_id)
```

Warehouse mount names are SQL aliases only. Moving data to another path creates
a different Dataset identity. `catalog://` is an alias type for Directory
resolution; it is not a `DatasetLocation` scheme.

## Read path

```text
path (after any Directory ticket or alias resolution)
  → resource-limited discovery
  → pin Snapshot
  → Source pruning and lazy open
  → normalized DataFusion relations
  → resource-limited CLI, API, or Web result
```

One operation pins each Source's version reference. Local files use identity
and fingerprint checks; Lance stores use their published generation or manifest;
object stores use a version or conditional ETag where available. The Snapshot
does not claim that unrelated Sources share a global transaction time.

Locate (`find`) and analyze (`query`) share that pinned Snapshot. CLI and Web
share the `find` expression, the reported scope, and `snapshot_id`. The Web UI
may highlight and clip returned fields; that frontend matching must not change
the match set. The Web Explorer is a UI over locate, not a separate query
language.

Normalized relations include `sources`, `runs`, `steps`, `tool_calls`, `events`,
and `trajectories` when supported by a Source. Every entity relation retains
`source_path`. SQL is read-only and rejects DDL, DML, network functions, and file
functions.

The detailed discovery and pruning algorithm belongs to
[Snapshot design](catalog.md).

## Write and publication path

Gateway and native writers publish canonical events before derived views.
Objects referenced by a Snapshot must be durable before the publication pointer
becomes visible. A failed publication leaves the previous Snapshot readable.

```text
validate event
  → persist payload or content-addressed object
  → append canonical fact
  → publish terminal fact or projection generation
  → expose through a later Snapshot
```

Writer concurrency is defined by the concrete store contract. Snapshot compare-
and-swap alone does not imply merge-and-retry behavior. Unpublished versions and
unreachable objects require an explicit maintenance path.

The canonical/projection boundary is detailed in
[Run storage](trajectory-storage.md); the Storyline implementation is
documented in [Storyline Lance](storyline-lance.md).

## Read-only Warehouse

The server mounts a static set of named paths. Refresh builds a complete new
Snapshot before switching readers. Dataset tables prune by Source before
opening matching fixed versions; caches and routing indexes are tied to that
Snapshot generation.

With `--catalog-config`, the parent process serves Directory list/ticket routes
and does not open those paths itself. Authorized Web queries run in a worker
that only receives the caller's paths. After a CLI ticket, the client opens the
ticket `uri` (a path) with storage credentials. That is platform addressing over
paths, not a new Dataset kind.

The Web application and API are consumers of the same read model. They do not
become another source of truth. Unknown API routes remain errors rather than SPA
fallbacks. Only loopback listeners are accepted.

User setup belongs to the [Warehouse guide](../guides/serve.md). Exact routes and
Gateway composition belong to the [`pchronicle` reference](../reference/cli.md).

## Guarantees and explicit non-guarantees

| Area | Guarantee | Non-guarantee |
| --- | --- | --- |
| identity | path + Source path + original ID remains visible | global uniqueness of external IDs |
| read consistency | fixed Source references within one Snapshot | global transaction across Sources |
| publication | previous Snapshot remains readable until new publication succeeds | automatic merge retry for every writer |
| query | resource-limited, read-only execution | arbitrary mutation or unlimited service query |
| projection | lineage and rebuildability where declared | projection freshness without a recorded generation |
| service | loopback-only static read surface; Directory mode authenticates the data plane with user keys | public multi-tenant Warehouse |

## Related design documents

- [Snapshot design](catalog.md): discovery, Snapshot construction, lazy Source
  resolution, and pruning.
- [RFC-0013 path Directory](../../rfcs/0013-pchronicle-warehouse-catalog.md):
  name-to-path resolution, ACL, tickets, and query workers.
- [Run storage](trajectory-storage.md): canonical facts, storage layouts, and
  write ownership.
- [Storyline Lance](storyline-lance.md): three-table projection, content layer,
  publication, and maintenance.
- [Recorded data, views, and versions](../concepts/facts-and-projections.md): the
  user-facing mental model behind these layers.
- [pChronicle Reference](../reference/index.md): exact CLI and format contracts.
