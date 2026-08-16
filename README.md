# Persisting

**Governed Agent execution and durable trajectory Datasets.**

Persisting has two connected product domains:

- pVisor virtualizes and governs Agent execution. pPilot extends the same Run
  contract to many independent Runs.
- pChronicle turns native and external trajectory Sources into durable,
  queryable Datasets with preserved origin, normalized views, and lineage.

They integrate through stable Run identity, canonical events, artifacts,
terminal facts, and Evidence, but each product also has a standalone entry
path.

Gateway, OverlayFS, OverlayNet, and Control are pVisor runtime drivers. Queue,
Search, and Tensor Memory are separate data capabilities; they are not
dependencies of the Agent execution path.

![Persisting product domains and integration](docs/src/assets/diagrams/persisting/system-products.svg)

## Install

The Python wheel installs the Python package and a matched set of three host
commands:

```bash
pip install persisting

pvisor --version
ppilot --version
pchronicle --version
```

The rolling nightly build is available without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

Source developers can install the same command set with `just install-cli`.
See the [installation guide](https://deeplink-org.github.io/Persisting/installation/)
for platform requirements and executor setup.

## Choose a product path

### Govern one Agent Run

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --all # or: pvisor drop last
```

`--safe` stages workspace changes. The exact filesystem and network boundary
is platform-dependent and is recorded in the Run Bundle; consult the
[pVisor guide](https://deeplink-org.github.io/Persisting/pvisor/guides/execution/)
before treating it as a security boundary. A useful Run Bundle is produced
without requiring pChronicle at runtime.

### Orchestrate many Runs

Create a Python plan with `plan()` and `execute(item)`, then run it with bounded
parallelism and a durable result journal:

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

For independent pVisor workspaces, use `ppilot produce`. Dataset queries and
analysis belong to pChronicle.

### Explore a trajectory Dataset

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source'
```

External Sources can enter pChronicle without pVisor. An explicit local path or
S3 URI can be supplied instead of configuring a default Warehouse.
`pchronicle import` and `export` exchange ATIF, ACTF, OpenAI Messages, and
Storyline JSON; `pchronicle serve` starts the loopback-only, read-only Warehouse
UI and API.

## pChronicle performance

Criterion.rs microbenchmarks and hyperfine lifecycle scenarios are compared
against `main` in CI. Measurements use stable JSONPath addresses in the raw
JSON report. See the [benchmark runner and report contract](benchmark/pchronicle/README.md).

<!-- pchronicle-benchmark:start -->
No nightly benchmark has been published with the unified report format yet.
<!-- pchronicle-benchmark:end -->

## Command ownership

| Command | Primary responsibility |
|---|---|
| `pvisor` | One Run, environments, review, checkpoints, apply/drop |
| `ppilot` | Batch planning, bounded execution, recovery, and Run production |
| `pchronicle` | Dataset catalog, SQL, built-in analysis, find, import/export, read-only serving |

## Current maturity

| Capability | Status |
|---|---|
| pVisor host execution, review, checkpoints, and transactional workspace | Implemented |
| pPilot batch orchestration and durable recovery | Implemented |
| pChronicle local/S3 catalog, bounded SQL, analysis, find, import/export | Implemented |
| pChronicle loopback-only read API and embedded Web UI | Implemented |
| Gateway capture and cooperative proxy policy | Implemented |
| Container/libkrun executors and transparent network boundaries | Platform-dependent; see the pVisor and OverlayNet docs |
| Queue and document Search | Separate stable capabilities |
| Tensor Memory / TTAS | Experimental |

The CLI `--help`, component READMEs, tests, and user guides describe supported
behavior. Files under `docs/src/design/` may also contain explicitly labelled
target architecture; RFCs preserve decisions and are not command references.

## Documentation

- [Persisting overview](https://deeplink-org.github.io/Persisting/overview/)
- [Run your first Agent](https://deeplink-org.github.io/Persisting/pvisor/get-started/)
- [pVisor documentation](https://deeplink-org.github.io/Persisting/pvisor/)
- [Review and selectively apply changes](https://deeplink-org.github.io/Persisting/pvisor/guides/review-apply/)
- [Orchestrate many Runs](https://deeplink-org.github.io/Persisting/pvisor/guides/orchestrate/)
- [pChronicle documentation](https://deeplink-org.github.io/Persisting/pchronicle/)
- [Explore durable history](https://deeplink-org.github.io/Persisting/pchronicle/get-started/)
- [Project architecture](https://deeplink-org.github.io/Persisting/system-design/)
- [Runnable examples](examples/)

From a source checkout:

```bash
just examples
just docs-build
just docs-links
```

## License

[Apache License 2.0](LICENSE). See [`NOTICE`](NOTICE) for third-party
attributions and separately licensed bundled components.
