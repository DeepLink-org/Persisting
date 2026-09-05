# Reproduce the Run lifecycle

The [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
directory is organized by product CLI. Each `run.sh` manages its own `.work/`
directory and reports durable outputs or query results. Together they follow
the documented sequence: execute, govern effects, and inspect history.

```bash
just examples
just examples-pvisor
just examples-pchronicle
```

## pVisor

| Example | What it demonstrates |
|---|---|
| `01-filesystem-isolation` | Transactional workspace isolation |
| `02-changeset-management` | Review, apply, and drop |
| `03-network-isolation` | Explicit proxy policy and its boundary |
| `04-gateway-llm-control` | Embedded Gateway routing and capture |

## pChronicle

| Example | What it demonstrates |
|---|---|
| `01-dataset-lifecycle` | Import, inspect, query, locate, and strictly export a Dataset |
| `02-built-in-analysis` | Summarize Sources, Agents, Models, and tools, then locate a Step |
| `03-cross-dataset-sql` | Run cross-Dataset SQL over three named Dataset mounts |
| `04-storage-query-performance` | Compare JSON/Lance size, compression, query ratios, and lifecycle latency |
| `05-format-roundtrip` | Strict ATIF roundtrip and canonical byte comparison |
| `06-query-openai-actf-directly` | Direct SQL over OpenAI Messages and ACTF Datasets |

The pChronicle examples use the deterministic fixtures in `examples/data`.
Their default output is a compact report; complete command stdout/stderr remains
under each scenario's `.work/run.*`, or can be expanded with
`PCHRONICLE_EXAMPLE_VERBOSE=1`.
Requirements are macOS or Linux, Cargo, Python 3, and common POSIX tools such
as `jq`. The pVisor filesystem examples additionally require macFUSE or FUSE3.
`just examples-pvisor-filesystem` runs the FUSE-backed 01/02 scenarios;
`just examples-pvisor-portable` runs 03/04 without FUSE.

Start with `pvisor/01-filesystem-isolation`, continue to changeset management,
then run the orchestration layer examples when you want many-Run production, and the
pChronicle examples when you are ready to inspect history.

Use [pVisor Guides](../pvisor/guides/index.md) for task explanations,
orchestration layer orchestration for many-Run workflows,
and [pChronicle Guides](../pchronicle/guides/index.md) for Dataset workflows.
The examples verify a product workflow; exact command syntax remains in each
product's Reference section.
