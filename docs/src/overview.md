# Persisting overview

Persisting has two connected entry paths: govern Agent execution with pVisor,
or build durable trajectory Datasets with pChronicle. Each path works on its
own, and stable contracts connect them when both products are used together.

## Two product domains

- **pVisor** virtualizes and governs Agent execution. **pPilot** extends the
  same Run contract to many independent Runs.
- **pChronicle** turns native and external trajectory Sources into durable,
  queryable Datasets with preserved origin, normalized views, and lineage.

The products meet at stable Run identity, canonical events, artifacts,
terminal facts, lineage, and Evidence. Neither product is only a stage in a
mandatory end-to-end lifecycle.

![Persisting product domains and integration](assets/diagrams/persisting/system-products.svg)

## Govern Agent execution

An Agent needs more than a process. It needs a workspace, tools, network
access, credentials, state, and a boundary around the changes it can make.
`pvisor` creates that boundary for one Run while the underlying host,
container, VM, or fleet resources remain shareable.

```bash
pvisor run --safe codex
```

The command uses a staged workspace and records the controls actually
installed. Run identity does not depend on a process ID or execution provider.
After the Run, inspect the result and accept only what should enter the base
workspace:

```bash
pvisor review last
pvisor apply last --path src
pvisor apply last --include 'tests/**'
pvisor apply last --all
```

`apply` is repeatable: each successful call consumes only the selected,
dependency-closed batch, while unselected changes remain staged. Gateway,
OverlayFS, OverlayNet, and Control are pVisor runtime drivers. pVisor produces
a useful Run Bundle without requiring pChronicle at runtime.

## Build trajectory Datasets

`pChronicle` discovers native and external trajectory Sources, preserves their
origin, records Catalog Snapshots, exposes normalized query and exchange views,
and retains revision lineage. External Sources can enter pChronicle without
first passing through pVisor.

```bash
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

The Dataset is the durable unit for discovery, inspection, exchange, and
analysis. pChronicle does not start, schedule, or control Agent Runs.

## Use the integrated path

When many governed Runs are needed, `pPilot` plans tasks, bounds concurrency,
fences leases, records durable results, and reconciles supported crash windows
without changing the Run contract:

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

pPilot scales pVisor Runs. Their canonical events, artifacts, terminal facts,
lineage, and Evidence can then become pChronicle inputs, joined through stable
Run identity rather than process or provider identity.

## Guarantees remain source-specific

pVisor records the controls and provider-specific Evidence available for each
Run; filesystem confinement does not imply network isolation or control of
remote Effects. pChronicle preserves the identity and lineage supplied by each
Source, but importing a trajectory cannot retroactively add execution controls
or Evidence that the Source did not carry.

## Continue by task

- [Run the first Agent](pvisor/get-started.md)
- [Review and selectively apply changes](pvisor/guides/review-apply.md)
- [Orchestrate many Runs](pvisor/guides/orchestrate.md)
- [Explore a trajectory Dataset](pchronicle/get-started.md)
- [Read the system architecture](system-design/index.md)
