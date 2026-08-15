# Dataset, Source, and Snapshot

pChronicle is path-first. It keeps the physical origin of history visible
instead of hiding every trajectory behind a global database identifier.

## Dataset

A Dataset is one logical query space rooted at a normalized local path or
object-store URI. The URI is its identity. A Warehouse mount name is only a SQL
alias and does not change that identity.

## Source

A Source is the smallest independently discovered and versioned trajectory
representation inside a Dataset. It may be a canonical event store, a
Storyline projection, or a supported exchange file. Every normalized row keeps
its `source_path`, so external IDs remain Source-local and collisions stay
visible.

The complete address of an entity is therefore:

```text
(dataset_uri, source_path, entity_kind, original_id)
```

## Catalog Snapshot

A Catalog Snapshot fixes the Source membership and version references used by
one operation. It guarantees that one query does not silently switch versions
mid-scan. It does not claim that unrelated Sources were produced at one global
instant.

Use the [Dataset workflow](../guides/discover-and-query.md) to inspect these
objects. The implementation is described by the
[Dataset Catalog design](../design/catalog.md).
