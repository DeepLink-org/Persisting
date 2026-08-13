# Persisting

**Agent execution, orchestration, and durable history.**

Persisting is an Agent infrastructure toolkit built from three peer products:

- **pVisor** runs one Agent in a reviewable workspace;
- **pPilot** plans, executes, and recovers many independent Runs;
- **pChronicle** discovers, queries, exchanges, and serves trajectory Datasets.

Gateway, Control, OverlayFS, and OverlayNet are runtime drivers assembled by
pVisor. Queue, Search, and Tensor Memory are separate data capabilities; they
are not dependencies of the Agent execution path.

```text
pPilot ── RunSpec ──► pVisor ── captured events ──► pChronicle
  │                    │                              ▲
  └──── results ───────┴──────────────────────────────┘
                       ├─ Control
                       ├─ OverlayFS
                       └─ OverlayNet → Gateway
```

## Install

The Python wheel installs the Python package and a matched set of four host
commands:

```bash
pip install persisting

persisting --version
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

## Start with the task

### Run and review one Agent

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last       # or: pvisor drop last
```

`--safe` stages workspace changes. The exact filesystem and network boundary
is platform-dependent and is recorded in the Run Bundle; consult the
[pVisor guide](https://deeplink-org.github.io/Persisting/guide/pvisor-execution/)
before treating it as a security boundary.

### Orchestrate many Runs

Create a Python plan with `plan()` and `execute(item)`, then run it with bounded
parallelism and a durable result journal:

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

For independent pVisor workspaces, use `ppilot produce`; for sharded analysis
or map/reduce processing, use `ppilot analysis` or `ppilot process`.

### Browse and analyze trajectories

```bash
# Optional: configure one local Dataset root as the default Warehouse.
pchronicle default ./trajectory-data

pchronicle ls
pchronicle status
pchronicle analysis overview
pchronicle query "SELECT agent_id, COUNT(*) AS runs FROM dataset.runs GROUP BY agent_id"
```

An explicit local path or S3 URI can be supplied instead of configuring a
default Warehouse. `pchronicle import` and `export` exchange ATIF, ACTF, OpenAI
Messages, and Storyline JSON; `pchronicle serve` starts the loopback-only,
read-only Warehouse UI and API.

## Command ownership

| Command | Primary responsibility |
|---|---|
| `pvisor` | One Run, environments, review, checkpoints, apply/drop |
| `ppilot` | Batch planning, bounded execution, recovery, distributed processing |
| `pchronicle` | Dataset catalog, SQL, built-in analysis, find, import/export, read-only serving |
| `persisting` | Compatibility and convenience entry point for execution, capture, event history, and evaluation |

`persisting query`, `persisting history`, `ppilot query`, `ppilot chronicle`,
and `ppilot convert` remain available for existing capture and conversion
workflows. New Dataset-oriented workflows should start with `pchronicle`.

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

## Documentation and examples

- [Quick Start](https://deeplink-org.github.io/Persisting/quickstart/)
- [Choose a capability](https://deeplink-org.github.io/Persisting/guide/)
- [pChronicle command reference](https://deeplink-org.github.io/Persisting/design/cli-pchronicle/)
- [Architecture and maturity](https://deeplink-org.github.io/Persisting/design/)
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
