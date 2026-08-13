# Reproducible Examples

The [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
directory is organized around product questions. Each `run.sh` is deliberately
linear: it clears `.work/`, runs pVisor or pPilot commands, and prints the
generated files, bundles, reports, or query results. The pChronicle benchmark
suite predates the standalone CLI and intentionally retains `ppilot chronicle`,
`ppilot convert`, and `ppilot query` as compatibility regression coverage.
For new Dataset workflows, use the [`pchronicle` command](../design/cli-pchronicle.md).

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
| [03-network-isolation](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/03-network-isolation) | Three direct commands verify allowlist, deny-all, and the direct-socket bypass boundary | [OverlayNet guide](overlaynet.md) |
| [04-gateway-llm-control](https://github.com/DeepLink-org/Persisting/tree/main/examples/pvisor/04-gateway-llm-control) | Gateway routes and captures two OpenAI-compatible calls | [Capture guide](capture.md) |

Here, lightweight isolation covers the transactional workspace and the data
plane visible to the cooperative public proxy. Direct sockets can bypass that
proxy. Host filesystem and deny-all network enforcement vary by platform; the
Run Bundle and pVisor isolation documentation describe the effective boundary.
The Gateway example includes its own mock model and two-turn Agent.

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

These examples use deterministic ATIF corpora and trimmed format fixtures to
compare physical size, analysis speed, SQL results, lossless peripheral format
recovery, and direct directory queries. Query performance uses Python's
standard `json.loads` plus an equivalent native loop as the raw-JSON baseline.
Direct pChronicle JSON queries and pChronicle Lance queries are independently
reported against that baseline. Results apply only to the printed dataset,
query, and machine.

| Example | What it demonstrates | Related guide |
|---|---|---|
| [01-atif-import-compression](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/01-atif-import-compression) | Physical size of raw JSON and the complete pChronicle Lance store | [Trajectory format](../design/trajectory-format.md) |
| [02-lance-vs-atif-speed](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/02-lance-vs-atif-speed) | Cold-process Python JSON baseline, pChronicle JSON, and pChronicle Lance queries | [Trajectory storage](../design/trajectory.md) |
| [03-analyze-lance-and-atif](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/03-analyze-lance-and-atif) | Semantic equivalence and one performance convention across all three paths | [pPilot CLI](../design/cli-ppilot.md) |
| [04-point-batch-live-query](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/04-point-batch-live-query) | Point-step, full-trajectory, CLI batching gain, and live canonical-event follow latency | [History CLI](../design/cli-history.md) |
| [05-format-roundtrip](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/05-format-roundtrip) | pPilot imports OpenAI/ACTF into three-table Lance and verifies lossless JSON-model recovery | [pPilot CLI](../design/cli-ppilot.md) |
| [06-query-openai-actf-directly](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/06-query-openai-actf-directly) | Direct OpenAI/ACTF directory SQL with `_file_ LIKE`, plus proof that Lance schemas stay unchanged | [pPilot CLI](../design/cli-ppilot.md) |
| [07-objects-lance-blob-offload](https://github.com/DeepLink-org/Persisting/tree/main/examples/pchronicle/07-objects-lance-blob-offload) | Inline/offloaded storage and query behavior for shared `objects.lance` blobs | [Trajectory storage](../design/trajectory.md) |

## Prerequisites

- macOS or Linux; Windows is not currently supported
- Cargo for compiling missing targets
- Python 3, `jq`, `awk`, and `curl`
- macFUSE on macOS or FUSE3 on Linux for pVisor filesystem examples
- no external network access for the network and Gateway examples

Install the CLI component set using the [installation guide](../installation.md),
or run `just install-cli` from a source checkout.
