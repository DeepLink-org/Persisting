# Design principles

## Keep facts and projections distinct

Canonical events preserve recorded facts. Normalized Storyline views and query
projections remain traceable to their Sources; missing records stay visible as
limits on the answer.

## Make ownership explicit

Shared event contracts describe logical records. pChronicle owns storage,
query, and exchange, while Gateway owns capture and protocol adaptation.
The CLI and Web UI use those boundaries rather than defining new data models.

## Preserve provenance and versions

An answer should identify the Dataset, Source, Snapshot, and query that produced
it. Revision lineage keeps derived outputs connected to their inputs across
normalization and export.

## Bound the work

Query budgets, bounded capture queues, and explicit append acknowledgements
make resource use and write outcomes inspectable. A failed projection must not
be reported as a successful durable append.

## Keep data portable

Documented formats and the CLI make Datasets usable without a particular
viewer. Local files and object-store Sources share the Dataset model.

See the [system overview](index.md) and [roadmap](../roadmap.md).
