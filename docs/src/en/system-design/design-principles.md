# Design principles

These principles explain why Persisting has separate products and why the
documentation emphasizes reviewable steps.

## Boundaries are explicit

pVisor describes the execution boundary that was actually installed. pChronicle
describes the Source and Dataset that were actually observed. Neither product
silently upgrades a missing control or incomplete Source into a stronger claim.

## Writes are reversible until reviewed

Agent Effects remain staged until a person or an explicit policy applies them.
Review is part of the workflow, not a report added after the write.

## Evidence travels with the result

A summary should point back to the Run, Dataset, Source, or query that produced
it. Lineage is useful only when it survives export, normalization, and later
inspection.

## Execution and history stay composable

pVisor can run without pChronicle, and pChronicle can analyze external Sources
without pVisor. The integration is a narrow capture contract so each product
remains useful on its own.

## Portable data beats a privileged viewer

Datasets, query results, and Run records should remain inspectable through the
CLI and documented formats. A web view can improve discovery, but it should not
be the only way to recover an answer.

See the [system overview](index.md) and the [roadmap](../roadmap.md) for how
these principles shape current delivery.
