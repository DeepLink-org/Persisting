# Persisting

**Persistent Infrastructure for the Agent Era.**

*From model state to Agent history.*

Persisting connects durable model state—parameters and KV caches—with durable
Agent history—trajectories and execution records. The positioning is broader
than any single command; the current product provides two concrete
workflows:

- `pvisor` runs one Agent in a controlled environment and lets you review its
  effects before accepting them;
- `pchronicle` browses, queries, exchanges, and serves trajectory Datasets.

Use either workflow on its own. pChronicle can read external trajectory data
without pVisor, and pVisor does not need pChronicle to complete its local
run-review-apply loop. When connected, they preserve a path from execution to
queryable history.

![Current Persisting workflows and optional integration](docs/src/assets/diagrams/persisting/system-products.svg)

## Install

The Python wheel installs the Python package and its matched public command-line
entry points:

```bash
pip install persisting

pvisor --version
pchronicle --version
```

The rolling nightly build is available without a Rust toolchain:

```bash
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

Source developers can install the same command set with `just install-cli`.
See the [installation guide](https://deeplink-org.github.io/Persisting/installation/)
for platform requirements and executor setup.

## Choose a workflow

### Run one Agent and review its changes

```bash
pvisor run --safe codex
pvisor review last
pvisor apply last --all # or: pvisor drop last
```

`--safe` stages workspace changes. The exact filesystem and network boundary
is platform-dependent and is recorded with the Run; consult the
[pVisor guide](https://deeplink-org.github.io/Persisting/pvisor/guides/execution/)
before treating it as a security boundary. This workflow does not require
pChronicle.

### Query trajectory history

```bash
pchronicle onboard
pchronicle onboard query
pchronicle agent codex ./trajectory-data \
  --ask "Which tools fail most often?"
```

The onboarding flow creates a temporary example Dataset and does not require a
source checkout. A Dataset may be a local path, an object-store URI prefix, or
a user alias such as `@prod`. `pchronicle import` accepts ATIF, ACTF, and OpenAI
Messages; `pchronicle export` supports those formats plus Storyline JSON.
`pchronicle agent` launches Codex or Claude with an ephemeral Dataset analysis
skill. It instructs the Agent to use read-only pChronicle commands without
changing the Agent's existing filesystem, network, or tool permissions.
`pchronicle serve` starts the loopback-only, read-only Dataset UI and API.

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
| `pchronicle` | Dataset catalog, SQL, built-in analysis, find, import/export, read-only serving |

## Current maturity

| Capability | Status |
|---|---|
| pVisor host execution, review, checkpoints, and transactional workspace | Implemented |
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

- [Choose a workflow](https://deeplink-org.github.io/Persisting/overview/)
- [Run your first Agent](https://deeplink-org.github.io/Persisting/pvisor/get-started/)
- [pVisor documentation](https://deeplink-org.github.io/Persisting/pvisor/)
- [Review and selectively apply changes](https://deeplink-org.github.io/Persisting/pvisor/guides/review-apply/)
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
