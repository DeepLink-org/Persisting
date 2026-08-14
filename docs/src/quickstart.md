# Quick Start

Follow one main path in five minutes: install the CLI, run an Agent safely,
orchestrate a batch, and query trajectory data with SQL. This guide assumes
macOS or Linux.

On macOS, install macFUSE once before using staged `--safe` Runs:

```bash
brew install --cask macfuse
```

## 1. Install the CLI

```bash
# Stable wheel: installs pchronicle, pvisor, and ppilot together
pip install persisting

# Or install the nightly wheel
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/Persisting/main/scripts/install-nightly.sh | bash
```

## 2. Run one Agent safely

Run the Agent from your project directory. Its workspace changes are staged
instead of being written directly to the project:

```bash
pvisor run --safe codex
```

`--safe` uses the current directory as the reusable workspace and OverlayFS
base, writes Agent changes to a per-Run stage, and enables the explicit network
proxy. The Run and its `run-bundle.json` remain under `PERSISTING_RUN_HOME`.

On Linux, the default Host executor uses a rootless user/mount namespace, a
minimal bind-projected root, and Landlock. Absolute paths, symlink escapes,
pathname Unix sockets, and descendants are confined to the staged workspace,
a read-only runtime, and explicitly granted paths. Setup
requires no root helper and fails closed if the kernel does not provide the
required controls. The default public proxy is still cooperative; pass
`--overlaynet-deny-all` when the Run must have no network namespace access.
On macOS, `--safe` installs a generated Seatbelt profile before Agent code
starts. Filesystem writes are confined to the staged workspace, explicit
read-write capabilities, and a Run-owned temporary directory; reads remain
ambient for local toolchain compatibility. `--overlaynet-deny-all` also blocks
IP and ambient host Unix sockets while retaining the Agent ABI and Run-local
IPC. Profile compilation or installation failure stops the Run, and the Run
Bundle reports read, write, and network enforcement separately.

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
pchronicle query examples/data/atif \
  'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source ORDER BY source'
```

`pchronicle query` accepts local Dataset directories and S3 URIs, discovers
supported ATIF, ACTF, OpenAI Messages, canonical events, and Storyline Sources,
and exposes normalized tables such as `runs`, `steps`, and `tool_calls`.
Try the built-in overview and inspect the discovered Sources:

```bash
pchronicle ls examples/data/atif
pchronicle analysis overview examples/data/atif
```

The pChronicle examples build Lance stores from fixtures and compare query
results. Some benchmark scripts deliberately exercise the older pPilot
compatibility commands as part of their regression contract:

```bash
just examples-pchronicle
```

## Other, independent capabilities

- [Queue](guide/queue.md) — persistent event streams

## Next steps

- [Installation](installation.md) — wheel, nightly, and source installation details
- [Choose a Capability](guide/index.md) — find the workflow for your goal
- [Architecture & Internals](design/index.md) — understand the implementation
