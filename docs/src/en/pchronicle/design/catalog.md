# Snapshot and Source discovery

`DatasetCatalogSnapshot` freezes the sources and versions discovered under
one or more named Dataset mounts. It provides a read boundary between storage
and DataFusion, and is rebuilt for an explicit refresh. It is distinct from
the optional Directory that authorizes and resolves Dataset paths.

## Discovery and identity

A Dataset is a local path or object-store URI. Sources include Storyline stores,
Compact JSONL datasets, and supported exchange files. `chronicle.manifest`
sidecars classify nested datasets and provide cached aggregate statistics.
Lance internals are opaque to recursive file discovery.

Entity identity is `(dataset path, source path, entity kind, original ID)`.
External IDs are not merged across sources. Rows retain `_file_` and
`document_id`; namespace aliases let one query join multiple mounts.

Discovery has explicit limits for files, bytes, parser concurrency, and error
policy. Local files are pinned by identity/fingerprint; remote objects use
version/ETag checks where available. Storyline stores pin their published
generation and exact table versions. Independent sources do not form a global
transaction.

## Lazy query path

```text
mounts → discover and pin → Dataset/_file_ pruning → lazy source open
       → normalized relations → bounded read-only DataFusion query
```

A lazy source caches its first resolution or failure within the Snapshot.
Storyline exposes `runs`, `steps`, and `tool_calls`; `trajectories` is the
compatibility view and `sources` describes physical sources. Exchange files
use the same normalized schema after decoding; Compact JSONL exposes record
relations. Predicate and column pushdown remain subject to each provider's
supported semantics.

The Warehouse builds a replacement Snapshot before switching readers. Its
summary and routing caches are tied to a Snapshot generation. A failed refresh
leaves the previous Snapshot readable. CLI queries and exports use their pinned
source references rather than a changing service cache.

See [Dataset, Source, and Snapshot](../concepts/dataset-and-source.md),
[query model](../reference/query-model.md), and [Storyline Lance](storyline-lance.md).
