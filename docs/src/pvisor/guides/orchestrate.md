# Orchestrate many Agent Runs

`pPilot` extends the Run model from one execution to a bounded collection of
tasks. It owns planning, concurrency, leases, retries for infrastructure
failures, durable result publication, and recovery.

It does not redefine the Agent runtime. Each task remains an independent Run.

## Define the work

Create `plan.py`:

```python
def plan():
    for value in range(6):
        yield {"id": f"square-{value}", "value": value}


def execute(item):
    value = item["value"]
    return {"square": value * value}
```

Stable IDs are important: retries and reconciliation use them to identify the
same logical task.

## Run with bounded concurrency

```bash
ppilot run plan.py --workers 2 --per-worker 2 --sink ./results
```

`--workers` and `--per-worker` bound active work. `--sink` enables the durable
result journal and lease fencing.

## Inspect durable results

```bash
cat ./results/ready.ndjson
```

Infrastructure failures may be retried. Business errors are reported rather
than silently retried. The reconciler repairs the supported crash windows
around result publication.

## Treat external effects explicitly

Lease fencing protects result ownership; it cannot make an arbitrary external
API exactly-once. Use stable task IDs as idempotency keys, or make the external
operation transactional or compensatable.

## Continue to history

The result sink is not trajectory history. Capture Agent events during each Run
and use [pChronicle](../../pchronicle/get-started.md) to inspect them across Runs.

For every runnable orchestration example, see [Reproducible examples](../../project/examples.md).
