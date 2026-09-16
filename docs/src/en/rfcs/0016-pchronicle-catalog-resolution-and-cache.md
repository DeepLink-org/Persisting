# RFC-0016: pChronicle Catalog Resolution and Manifest Cache

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Date** | 2026-09-13 |
| **Component** | `persisting-pchronicle`, `pchronicle` CLI, Warehouse Datasets UI |
| **Related** | [RFC-0013 path Directory](0013-pchronicle-warehouse-catalog.md) · [RFC-0015 `chronicle.manifest`](0015-chronicle-manifest.md) |

## Summary

pChronicle has one catalog boundary for locating and describing data. A
`DatasetLocation` identifies a physical location, a `Dataset` identifies one
atomic dataset, and a `DatasetMount` identifies a path whose descendants form
an aggregate query space. Resolution and manifest caching live in the core
`persisting-pchronicle` crate and are shared by the CLI, Web UI, and query
engine.

The cache is an acceleration layer for discovery and navigation. It MUST NOT
change the correctness contract of queries or CLI operations.

## Terminology and model

- **Location**: a parsed local or object-store URI. It owns one-level listing
  and manifest reads; it does not decide how a caller uses the result.
- **Dataset**: one atomic, queryable leaf. Its membership and revision are
  determined by the authoritative dataset layout and its `chronicle.manifest`
  when present.
- **Dataset mount**: a named path that may contain many datasets. It is a
  navigational and query boundary, not an additional physical dataset.
- **Manifest listing**: one cached `list(prefix)` observation, including child
  kinds and optional record statistics.

The model is deliberately separate from DataFusion. DataFusion receives a
resolved dataset or mount and applies mount-specific pruning; it does not own
filesystem traversal or the UI cache.

## Resolver contract

All callers use the shared resolver and select an explicit read mode:

| Mode | Purpose | Authoritative I/O |
|---|---|---|
| `Cached` | Render an existing UI view immediately | Never |
| `RefreshIfMissing` | Fill a missing cache entry | Only on a miss |
| `Fresh` | CLI, mutations, and correctness-sensitive operations | Always |

The resolver MUST normalize prefixes, reject traversal components, preserve
the distinction between an atomic Dataset and a Dataset mount, and keep URI
identity in cache keys. A cached result MAY be stale or incomplete; a fresh
result MUST reflect the authoritative location at the time of resolution.

CLI catalog calculations and query planning use `Fresh`. The Web UI uses
`Cached` first and refreshes in the background. UI cache data MUST NOT be used
to answer a user query, establish source membership, or select a revision.

## Manifest cache

`ManifestCache` is the single owner of manifest observations. Callers do not
open `chronicle.manifest` or maintain a second catalog cache themselves.

Each entry is keyed by a stable mount identity and normalized prefix:

```text
dataset-name NUL location-fingerprint [NUL prefix]
```

The cache has an in-memory read path backed by a persistent Lance table under
the pChronicle cache directory. Opening a corrupt, incompatible, or
unreadable cache MUST rebuild it and continue with an empty cache. Cache
errors MUST NOT make `serve` fail when the authoritative location is still
available.

The Warehouse separates interactive directory browsing from manifest maintenance:

| Work | Remote I/O | Local result | Admission and backoff |
|---|---|---|---|
| `GET /api/explorer/tree` | One-level LIST on a directory cache miss or refresh; no remote manifest probes | Names enriched from `ManifestCache` with Dataset types and trajectory counts | Foreground AIMD, scoped by endpoint + bucket |
| Background manifest worker | Probe the current prefix and list immediate children, then walk breadth first | Update persistent manifest observations and the browse projection | Independent background AIMD, also scoped by endpoint + bucket |

Foreground requests use an existing directory projection or manifest observation
immediately. A cold request starts a shallow LIST and waits up to 250 ms before
returning an explicit loading view. Metadata that has not been observed is
partial, not a certified zero count. Background work is bounded to 32 prefixes
per 30-second round and a 10,000-directory frontier; unfinished observations
remain partial. Each refresh job has a 10-second deadline.

The two workloads do not share in-flight deduplication or failure cooldowns.
A foreground LIST of a prefix can run while that same prefix's background
manifest request is stalled. They may reuse an OpenDAL client, but their AIMD
semaphores, failure counters and cooldowns are independent. Each workload
continues to share admission across paths on the same endpoint and bucket.

A successful observation updates memory and attempts persistent storage. A
failed observation retains the previous cache. The directory projection stores
navigation data; Dataset identity and aggregate counts come only from the
local manifest observations, scoped to the selected mount and prefix.

## Aggregation semantics

For a mount or prefix, cached summaries include Dataset leaves observed at that
prefix and all cached descendant prefixes. They report dataset count and
trajectory count when those values are present in the manifest. They are
best-effort UI metadata and can temporarily be zero while a cold cache is
warming.

Directory cards may display the summary for their child prefix. A directory
without a cached descendant observation remains navigable and displays a
neutral directory label until background refresh discovers its contents.

Refreshes also remove descendants that disappear from a successfully listed
parent. Failed or unavailable parent reads MUST NOT erase an existing cached
view.

## Query behavior for mounts

The query engine MAY accept either an atomic Dataset or a Dataset mount. For a
mount it MUST push path and dataset predicates down before constructing
cross-dataset joins, prune datasets that cannot satisfy the predicate, and
avoid materializing unrelated descendants. These optimizations are part of
query execution, not manifest-cache correctness, and must remain valid when
the cache is stale.

## Consistency and recovery

The cache is rebuildable. It is safe to delete the local cache and restart;
the next foreground request or periodic walk repopulates it. Persistent cache
writes are best effort for UI use, while fresh CLI and query paths surface
authoritative read errors.

This design intentionally does not promise a single global snapshot across
multiple object-store locations. Each manifest listing records its own
observation time; a UI response identifies itself as best effort and exposes
refreshing, stale, and last-error state.

## Rejected alternatives

- Re-scanning every manifest for every UI request: predictable worst-case
  latency and excessive object-store I/O.
- A CLI-only cache: duplicates resolution rules and allows UI, CLI, and query
  semantics to drift.
- Treating a mount as one physical Dataset: hides membership boundaries and
  encourages unbounded joins.
- Using cached membership for accurate queries: stale caches can omit newly
  created datasets or retain removed ones.
