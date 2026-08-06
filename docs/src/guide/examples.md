# Reproducible Examples

The [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
directory is organized around product questions. Each `run.sh` is deliberately
linear: it clears `.work/`, runs pVisor or pPilot commands, and prints the
generated files, bundles, reports, or query results. The pChronicle examples
enter through `ppilot chronicle` and `ppilot query`, not internal Rust examples.

## Run the examples

```bash
just examples                 # all examples
just examples-pvisor          # four pVisor examples
just examples-pchronicle      # pChronicle examples
just examples-ppilot          # pPilot examples
```

Every entry point incrementally builds and uses release Rust targets; later
runs reuse the Cargo cache. Requirements are macOS or Linux, Cargo, Python 3,
and common POSIX tools such as `jq`, `awk`, and `curl`.

## pVisor: lightweight isolation and Run control

These examples demonstrate a transactional workspace, changeset management,
an explicit network proxy, and Gateway capture. The filesystem examples need
macFUSE on macOS or FUSE3 on Linux.

| Example | What it demonstrates | Related guide |
|---|---|---|
| [01-filesystem-isolation](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/01-filesystem-isolation) | Agent writes land in the upper while the lower remains unchanged | [pVisor CLI](../design/cli-pvisor.md) |
| [02-changeset-management](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/02-changeset-management) | A changeset can be reviewed, applied, or dropped | [pVisor CLI](../design/cli-pvisor.md) |
| [03-network-isolation](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation) | Three direct commands verify allowlist, deny-all, and the direct-socket bypass boundary | [OverlayNet design](../design/overlaynet.md) |
| [04-gateway-llm-control](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/04-gateway-llm-control) | Gateway routes and captures two OpenAI-compatible calls | [Capture guide](capture.md) |

Here, lightweight isolation covers the transactional workspace and the data
plane visible to the cooperative proxy. The Run Bundle reports that the Host
executor can still access paths outside the workspace and that direct sockets
can bypass the explicit proxy. The Gateway example includes its own mock model
and two-turn Agent.

## pPilot: batch orchestration and trajectory processing

These examples cover pPilot's four public modes with local Pulsing workers;
they do not require `torchrun` or a multi-node environment.

| Example | What it demonstrates | Related guide |
|---|---|---|
| [01-run](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/01-run) | `plan()` and `execute()` run concurrently and write a durable sink | [Quick Start](../quickstart.md) |
| [02-produce](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/02-produce) | A Python planner creates independent, reviewable pVisor Runs | [pPilot CLI](../design/cli-ppilot.md) |
| [03-process](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/03-process) | Python map/reduce combines deterministic ATIF shards | [pPilot CLI](../design/cli-ppilot.md) |
| [04-analysis](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/04-analysis) | Read-only SQL runs over balanced ATIF shards and merges the rows | [pPilot CLI](../design/cli-ppilot.md) |

The command is named `produce`, for producing a batch of trajectory Runs.

## pChronicle: trajectory storage and analysis

These examples use the same deterministic ATIF corpus to compare physical
size, analysis speed, and SQL results across formats. Size and speed numbers
apply only to the printed dataset, query, and machine; they are not universal
claims about Lance or ATIF.

| Example | What it demonstrates | Related guide |
|---|---|---|
| [01-atif-import-compression](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/01-atif-import-compression) | pPilot reports the size ratio, saved space, and compression factor | [Trajectory format](../design/trajectory-format.md) |
| [02-lance-vs-atif-speed](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/02-lance-vs-atif-speed) | End-to-end pPilot CLI import, replacement, and cold Lance/ATIF query latency | [Trajectory storage](../design/trajectory.md) |
| [03-analyze-lance-and-atif](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/03-analyze-lance-and-atif) | pPilot explicitly reports cross-backend SQL equivalence | [pPilot CLI](../design/cli-ppilot.md) |

## Prerequisites

- macOS or Linux; Windows is not currently supported
- Cargo for compiling missing targets
- Python 3, `jq`, `awk`, and `curl`
- macFUSE on macOS or FUSE3 on Linux for pVisor filesystem examples
- no external network access for the network and Gateway examples

Install the CLI component set using the [installation guide](../installation.md),
or run `just install-cli` from a source checkout.
