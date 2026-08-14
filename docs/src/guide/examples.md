# Reproducible Examples

The [`examples/`](https://github.com/DeepLink-org/Persisting/tree/main/examples)
directory is organized by product CLI. Each `run.sh` clears its own `.work/`
directory and reports durable outputs or query results.

```bash
just examples
just examples-pvisor
just examples-ppilot
just examples-pchronicle
```

## pVisor

| Example | What it demonstrates |
|---|---|
| `01-filesystem-isolation` | Transactional workspace isolation |
| `02-changeset-management` | Review, apply, and drop |
| `03-network-isolation` | Explicit proxy policy and its boundary |
| `04-gateway-llm-control` | Embedded Gateway routing and capture |

## pPilot

| Example | What it demonstrates |
|---|---|
| `01-run` | Concurrent `plan()` / `execute()` with a durable sink |
| `02-produce` | A streaming planner creates independent pVisor Runs |

## pChronicle

| Example | What it demonstrates |
|---|---|
| `05-format-roundtrip` | Strict ATIF roundtrip and canonical byte comparison |
| `06-query-openai-actf-directly` | Direct SQL over OpenAI Messages and ACTF Datasets |

The pChronicle examples use the deterministic fixtures in `examples/data`.
Requirements are macOS or Linux, Cargo, Python 3, and common POSIX tools such
as `jq`. The pVisor filesystem examples additionally require macFUSE or FUSE3.
