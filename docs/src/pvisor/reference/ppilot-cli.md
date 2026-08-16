# `ppilot` command reference

`ppilot` is the scalable Run-production CLI. It exposes exactly two commands:

```text
ppilot
├── run       execute plan() / execute(item) with durable recovery
└── produce   create independent pVisor Runs from a streaming planner
```

Dataset discovery, SQL, built-in analysis, find, import/export, and serving are
owned by [`pchronicle`](../../pchronicle/reference/cli.md).

## `run`

```bash
ppilot run plan.py --workers 8 --per-worker 2 --sink ./results
ppilot run plan.py --workers 8 --sink ./results --resume
ppilot run plan.py --check
ppilot run plan.py --pvisor-binary ./target/release/pvisor
```

The script defines `plan()` and `execute(item)`. pPilot applies bounded
concurrency and backpressure, writes terminal results to the durable sink, and
uses stable task identity for resume and retry. `--check` validates the plan
and a sample execution without running the full workload.

## `produce`

```bash
ppilot produce production.py --output ./runs --parallelism 8
ppilot produce production.py --output ./runs --parallelism 8 \
  --cluster-network-limit 10mbps -- --dataset train
```

The planner's `plan()` may be a synchronous or asynchronous iterator. Each
item describes one Run:

```python
def plan():
    for index in range(100):
        yield {
            "id": f"task-{index:04d}",
            "agent": "codex",
            "command": ["codex", "exec", f"Solve task {index}"],
            "cwd": "/work/eval",
        }
```

Each emitted item gets its own pVisor workspace below `--output`. The planner
is streamed under the concurrency window, so large batches are not fully held
in memory. The command writes `production-report.json`; any failed Run makes
the command exit unsuccessfully after the report is durable.

`--cluster-network-limit` divides a conservative aggregate proxy rate across
the requested parallelism. It requires Gateway capture and does not cover
direct sockets that bypass the explicit proxy.

## Runtime ownership

Both commands start an in-process, job-scoped Supervisor. pPilot owns planning,
leases, retries, reconciliation, and collection. pVisor owns Run execution and
the embedded Gateway. pPilot invokes one foreground `pvisor` process per Run;
the two components share Run and Supervisor contracts through agentctl rather
than linking pVisor into pPilot. `--pvisor-binary` and
`PERSISTING_PVISOR_BIN` select an explicit executable. pChronicle owns
trajectory Dataset operations.

The executable's `--help` is authoritative for flags and defaults.

Use [Orchestrate many Agent Runs](../guides/orchestrate.md) for the complete
workflow, [pPilot architecture](../design/orchestration.md) for leases and
reconciliation, and [Run, Attempt, and Effect](../concepts/run-model.md) for the
retry identity model.
