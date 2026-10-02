# Troubleshoot a Dataset

Diagnose a pChronicle result in the same order every time: confirm the path,
inspect what is visible, then narrow the query. This keeps a missing Dataset,
an empty result, and a resource limit from looking like the same failure.

## Confirm the Dataset first

Use a concrete path while investigating. A pin adds one more resolution step:

```bash
pchronicle dataset list
pchronicle stats ./trajectory-data --format json
pchronicle list ./trajectory-data --format json
```

If a pin fails, resolve the pin before debugging storage credentials or SQL:

```bash
pchronicle dataset show prod
pchronicle stats @prod --format json
```

A pin points to a Dataset; it does not copy or move the underlying data.

## The Dataset opens but appears empty

Check the summary before writing a more selective query:

```bash
pchronicle stats overview ./trajectory-data
pchronicle find ./trajectory-data --match "" --format json
```

An empty result can mean that the path contains a supported format with no
matching records, that a filter is scoped to the wrong entity, or that the
Dataset contains files pChronicle does not recognize. The overview and JSON
metadata identify the visible sources and the search mode.

## A query returns no rows

Start with a bounded count, then inspect the normalized table names:

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT COUNT(*) AS runs FROM dataset.runs'
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

Use `find` for identity or text discovery before composing a join. A Snapshot
pins one read view; if data changes between two commands, record the Snapshot
identifier from the JSON output and reuse it in the follow-up query.

## The query stops at a limit

Resource limits are part of the public query contract. Reduce the question
before raising a limit:

```bash
pchronicle query ./trajectory-data \
  --sql 'SELECT source, COUNT(*) FROM dataset.steps GROUP BY source' \
  --max-output-rows 20 --timeout 10s
```

Use `--file` for a checked-in query and explicit output limits in CI. A query
that needs a larger budget should explain why in the calling workflow rather
than silently removing the guard.

## The source format is unsupported

Check the [supported formats](../reference/formats/index.md) and use the
exchange guide to import into a Dataset pChronicle can normalize. Import does
not invent missing lineage or Evidence; preserve the original Source alongside
the normalized view when provenance matters.

## Verify the local block cache

Remote Lance reads use a persistent, version-keyed disk cache. Blocks are 1 MiB
by default; range and full-file reads share the same blocks. Reopening a Dataset
in the same process reuses metadata and concurrent downloads. Cache hits update
persisted access times, so eviction retains recently used blocks after a worker
restart. At 90% capacity, background eviction targets 80% usage, leaving room for new downloads.

| Environment variable | Default | Purpose |
| --- | --- | --- |
| `PCHRONICLE_LANCE_CACHE_DIR` | OS cache directory, under `pchronicle/blocks` | Shared cache location; also honored by catalog workers |
| `PCHRONICLE_LANCE_CACHE_CAPACITY_BYTES` | `536870912` (512 MiB) | Disk capacity target |
| `PCHRONICLE_LANCE_CACHE_BLOCK_SIZE_BYTES` | `1048576` (1 MiB) | Aligned download/cache block size |

Run the same remote query twice with debug logging:

```bash
pchronicle --log-level debug query @prod \
  --sql 'SELECT COUNT(*) FROM dataset.steps'
```

Look for `pchronicle.block_cache`: `block cache miss` records downloaded blocks,
`block cache hit` records disk reads, and `block cache status` reports hits,
misses, hit/downloaded bytes, coalesced reads, evictions, write errors and disk
usage at the latest reconciliation. `CachedObjectStore::stats()` exposes the
same counters, shared by stores with the same cache configuration in a process.
A second CLI process reuses disk blocks but still discovers remote metadata.

Metadata is reused for up to 60 seconds. Explicit HEAD requests and Lance's
mutable version hint retain remote freshness; successful writes, multipart
completion, copies and deletes invalidate remembered metadata. Local disk write
failures are logged and the downloaded bytes are still returned. A block larger
than the configured capacity is served without caching.

This uses the aligned-block and persistent LRU principles described in
[Doris file cache internals](https://doris.apache.org/docs/3.x/compute-storage-decoupled/file-cache/file-cache-internals/).
Eviction reconciles disk contents periodically, including files written by other
workers. Downloads are coalesced within a process; separate processes can still
download the same cold block concurrently. Priority queues and prewarming are
not implemented.

## Before opening an issue

Include the pChronicle version, Dataset path or pin name (without credentials),
the output of `status --format json`, the exact query, and its resource limits.
For object storage, include the provider type and region or endpoint, never
access keys or signed URLs.
