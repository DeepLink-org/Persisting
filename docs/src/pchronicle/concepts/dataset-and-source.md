# Dataset, Source, and Snapshot

pChronicle is an Agent trajectory storage engine. A Dataset is a **path**: the
physical origin of trajectory data stays visible instead of hiding every
trajectory behind a global database identifier.

## Dataset

A Dataset is one query space rooted at a normalized local path or object-store
URI. The path is its identity. A Warehouse mount name is only a SQL alias.
An `@alias` or Directory library name is a locator; after resolution the engine
opens the path.

A Dataset is a discovery, Snapshot, query, and exchange boundary. It does not
claim that every expected external task produced a Source. pChronicle reports
the Sources it can discover and pin; it does not infer unreported trajectories.

## Source

A Source is the smallest independently discovered and versioned trajectory
representation inside a Dataset. It may be a canonical event store, a
Storyline projection, or a supported exchange file. Every normalized row keeps
its `source_path`, so external IDs remain Source-local and collisions stay
visible.

The complete address of an entity is therefore:

```text
(path, source_path, entity_kind, original_id)
```

## Snapshot

A Snapshot is the sync protocol between writers and readers after a path is
opened. It fixes the Source membership and version references used by one
operation so a query does not silently switch versions mid-scan. It does not
claim that unrelated Sources were produced at one global instant.

Directory listing and tickets (RFC-0013) are not Snapshots. They only decide
which paths a caller may open.

Use the [Dataset workflow](../guides/discover-and-query.md) to inspect these
objects. The implementation is described by the
[Snapshot design](../design/catalog.md).
