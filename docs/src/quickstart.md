# Quick Start

Follow one main path in five minutes: install the CLI, run an Agent safely,
orchestrate a batch, and query trajectory data with SQL. This guide assumes
macOS or Linux.

## 1. Install the CLI

```bash
# From source, for development
git clone https://github.com/DeepLink-org/Persisting.git
cd Persisting
just install-cli

# Or install nightly binaries without a Rust toolchain
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-cli-nightly.sh | bash
# Add ~/.persisting/cli/bin to PATH as instructed by the installer
```

## 2. Run one Agent safely

Run the Agent from your project directory. Its workspace changes are staged
instead of being written directly to the project:

```bash
pvisor run --safe codex
```

`--safe` uses the current directory as the OverlayFS lower, writes Agent
changes to a staged upper, and enables the explicit network proxy. The
workspace and its `run-bundle.json` remain after the command exits.

The default Host executor provides process-level isolation. It does not stop
the Agent from accessing host paths outside the project, and a direct socket
can bypass the explicit proxy. The Run Bundle reports these boundaries.

```bash
pvisor review last     # inspect file changes, network counters, and warnings
pvisor apply last      # accept the changes
# or
pvisor drop last       # discard the changes
```

## 3. Orchestrate a batch

Create `plan.py`. `plan()` yields tasks with stable IDs, and `execute(item)`
handles one task:

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    value = item["value"]
    return {"square": value * value}
```

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results
cat ./results/ready.ndjson
```

`--sink` enables a durable result journal and lease fencing. Retries return to
the original slot, business errors are not retried automatically, and the
reconciler repairs the two supported crash windows. Use stable IDs to make
external side effects idempotent.

The runnable version is under `examples/ppilot/01-run/`. The other pPilot
examples demonstrate `produce`, `process`, and `analysis`:

```bash
just examples-ppilot
```

## 4. Query trajectory history

The result sink above contains task results; it is not a trajectory store.
From a Persisting source checkout, query the bundled ATIF trajectory fixtures
directly:

```bash
ppilot query crates/persisting-pchronicle/tests/fixtures/atif \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'
```

`ppilot query` accepts ATIF JSON, JSONL, or directories as well as local and
`s3://` Lance Storyline stores. Both sources expose the `runs`, `steps`, and
`tool_calls` tables. The pChronicle examples build a Lance store from those
same fixtures and compare the query results:

```bash
just examples-pchronicle
```

## Other capabilities

- [Tensor Memory (experimental)](guide/tensor-memory.md) — tensor subscripts and tiered storage
- [Queue](guide/queue.md) — persistent event streams
- [Search](guide/search.md) — document indexing and vector/hybrid retrieval

## Next steps

- [Installation](installation.md) — details for all three distributions
- [Choose a Capability](guide/index.md) — find the workflow for your goal
- [Architecture & Internals](design/index.md) — understand the implementation
