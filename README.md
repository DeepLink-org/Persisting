# Persisting

**Virtualized Agent execution, governed effects, and durable history.**

Persisting turns an Agent command into a durable **Run** with an isolated
virtual execution environment, reviewable effects, stable identity, and
history that survives the process.

The product follows one lifecycle:

1. **pVisor** creates an Agent virtual execution environment for one Run.
2. The Agent works inside a staged boundary; the user reviews and selectively
   accepts its effects.
3. **pPilot** plans, executes, and recovers many independent Runs without
   changing the Run model.
4. **pChronicle** preserves, queries, exchanges, and serves the resulting
   trajectory history.

Gateway, Control, OverlayFS, and OverlayNet are runtime drivers assembled by
pVisor. Queue, Search, and Tensor Memory are separate data capabilities; they
are not dependencies of the Agent execution path.

![One execution model from laptop to fleet](docs/src/assets/diagrams/persisting/execution-story.svg)

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

## Follow the Run lifecycle

### Run and review one Agent

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --path src
pvisor apply last --include 'tests/**'
pvisor apply last --all # or: pvisor drop last
```

`--safe` stages workspace changes. The exact filesystem and network boundary
is platform-dependent and is recorded in the Run Bundle; consult the
[pVisor guide](https://deeplink-org.github.io/Persisting/pvisor/guides/execution/)
before treating it as a security boundary.

### Orchestrate many Runs

Create a Python plan with `plan()` and `execute(item)`, then run it with bounded
parallelism and a durable result journal:

```bash
ppilot run plan.py --workers 4 --per-worker 2 --sink ./results
```

For independent pVisor workspaces, use `ppilot produce`. Dataset queries and
analysis belong to pChronicle.

### Browse and analyze trajectories

```bash
# Start with a temporary guided walkthrough, or jump to its SQL section.
pchronicle onboard
pchronicle onboard query

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
