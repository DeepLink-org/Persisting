# Get Started with pPilot

This page runs the shortest verified pPilot loop: a streaming Python plan
executed by multiple workers, with terminal results written to a durable sink.

## Install

`ppilot` ships in the same Python wheel as `pvisor` and `pchronicle`:

```bash
pip install persisting[lance]
ppilot --version
```

From a source checkout, `just install-cli` installs the same component set.

## Define the work

Create `plan.py`:

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    return {"square": item["value"] ** 2}
```

`plan()` yields work items with stable `id`s; `execute(item)` processes one
item. Stable identity lets an interrupted job resume without repeating
completed work.

## Run it

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results --results ndjson
```

## Verify the durable result

```bash
cat ./results/ready.ndjson
```

Expected: six result records, one per task, with squares 0, 1, 4, 9, 16, 25
(sum 55). A scripted version of this loop lives in
[`examples/ppilot/01-run/`](https://github.com/DeepLink-org/Persisting/tree/main/examples/ppilot/01-run).

## Where to go next

- [Orchestrate many Agent Runs](guides/orchestrate.md) — resume, retries, and production sinks
- [pPilot CLI reference](reference/cli.md) — `run` and `produce` flags
