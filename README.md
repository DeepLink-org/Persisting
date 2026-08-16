# Persisting

**Governed Agent execution and durable trajectory Datasets.**

Persisting has two connected product domains:

- pVisor virtualizes and governs Agent execution. pPilot extends the same Run
  contract to many independent Runs.
- pChronicle turns native and external trajectory Sources into durable,
  queryable Datasets with preserved origin, normalized views, and lineage.

The domains share stable Run identity where it is present. Today the configured
pVisor-to-pChronicle path publishes Gateway trajectory events and pVisor
lifecycle records, including the Evidence those records carry. The private Run
Bundle, its Artifact references and lineage, staged Effects, and broader runtime
Evidence remain local unless a separate adapter moves them.

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
pchronicle onboard
pchronicle onboard query
```

The installed-product onboarding flow creates temporary example Datasets and
does not require a source checkout. External Sources can enter pChronicle
without pVisor. `pchronicle import` accepts ATIF, ACTF, and OpenAI Messages;
`pchronicle export` supports those formats plus Storyline JSON.
`pchronicle serve` starts the loopback-only, read-only Warehouse UI and API.

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
