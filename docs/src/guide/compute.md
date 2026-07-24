# Compute Orchestration

> **Status**: Stable — `plan()` + `execute()` model, local parallelism and torchrun.

Sometimes you need to run the same function over many inputs — hyperparameter sweeps, evaluation batches, data processing pipelines. You could write a `for` loop. But then you need parallelism. And checkpointing. And resume after interruption. And multi-machine scale-out.

Persisting Compute gives you all of that with one constraint: write a Python file with two functions — `plan()` (what to do) and `execute(item)` (how to do it).

---

## Your First Task

A compute task is a single Python file. `plan()` yields task dicts. `execute(item)` processes one and returns a result.

```python
import argparse

def _parse_args(argv=None):
    p = argparse.ArgumentParser()
    p.add_argument("-n", "--n", type=int, default=10)
    return p.parse_args(argv)

def plan():
    """Yield tasks. Each must have an 'id'."""
    args = _parse_args()
    for i in range(args.n):
        yield {"id": f"t-{i}", "x": i}

def execute(item):
    """Process one task. Returns a dict."""
    return {"x2": item["x"] * 2}

# Works as a plain script too
if __name__ == "__main__":
    for xx in plan():
        print(execute(xx))
```

The contract is simple:

- `plan()` yields dicts — each must have an `id` (UUID generated if missing)
- `execute(item)` receives exactly what `plan()` yielded
- Arguments go after `--` — same as `python task.py --n 2`

---

## From Loop to Parallel

Validate locally first, then scale:

```bash
# Does it even work?
python3 task.py --n 2
persisting compute task.py --check -- --n 2

# Run with 4 local workers
persisting compute task.py -w 4 -- --n 100

# Or fewer workers, more concurrency per worker
persisting compute task.py -w 1 --per-worker 4 -- --n 100
```

The same file scales to multi-process without changes:

```bash
torchrun --nproc_per_node=8 -- persisting compute task.py -- --n 100
```

The key difference from writing your own multiprocessing: **tasks are dispatched exactly once, even across workers and across restarts.**

---

## Surviving Interruptions

Add `--sink` and your results are durable:

```bash
persisting compute task.py -w 4 --sink /tmp/run1 -- --n 1000
```

The sink directory contains:

| File | Contents |
|------|----------|
| `ready.ndjson` | Successful results, one JSON per line |
| `failures.ndjson` | Failed or cancelled tasks |
| `checkpoint.json` | Progress summary |

If the job is interrupted (Ctrl-C, node failure), resume picks up where it left off:

```bash
persisting compute task.py -w 4 --sink /tmp/run1 --resume -- --n 1000
```

Resume skips any `task_id` already present in `ready.ndjson` or `failures.ndjson`. `plan()` runs again but completed tasks are never re-dispatched. To re-run failures, clear `failures.ndjson` or use a fresh sink directory.

Progress monitoring:

```bash
persisting compute task.py -w 4 --sink /tmp/run1 --resume --observe -- --n 1000
```

---

## How Dispatch Works

The compute runtime follows a simple model:

```
plan()  ──streams──►  Driver  ──dispatches──►  Workers (Python hosts)
                         │                          │
                         │  sticky: once a task     │  each worker is a
                         │  hits a worker, all      │  long-lived Python
                         │  retries go there         │  subprocess
                         │                          │
                         ▼                          ▼
                      SkipSet                   execute(item)
                   (already done ids)                │
                                                    ▼
                                               ResultSink
                                          (ready.ndjson + failures)
```

- **SkipSet**: After resume, `plan()` still yields all tasks, but the Driver skips any with IDs already in the sink.
- **Sticky dispatch**: If a worker fails mid-task, retries go to the same worker (avoids at-least-once re-execution on a different worker).
- **Quarantine**: If a worker fails repeatedly, it's quarantined — no new tasks are sent there.
- **ResultCache**: If a worker restarts, the Driver can retrieve already-computed results without re-running `execute`.

---

## Common Options

| Option | Description |
|--------|-------------|
| `-w N` | Local worker count |
| `--per-worker N` | Concurrent slots per worker (default 1) |
| `--python PATH` | Python interpreter |
| `--retries N` | Infrastructure retries (worker unreachable, not business failures) |
| `--results ndjson\|summary\|quiet` | Output format |
| `--sink DIR` | Persist results |
| `--resume` | Skip completed task IDs |
| `--observe` | Show progress |
| `--traj` | Also write results as Vortex trajectory |

---

## FAQ

**Task didn't run / duplicate IDs?**  
Same `id` only dispatched once per job. Resume skips sink-existing IDs.

**After Ctrl-C?**  
Unstarted tasks marked cancelled. In-flight `execute` interrupted. Written results preserved.

**Does `--retries` cover business failures?**  
No. Retries are for infrastructure (worker unreachable). Handle business retries in `execute`.

**When to use vs Ray / multiprocessing?**  
Use Compute when you want `plan()` + `execute()` without bringing in a distributed framework. Best for file/shard-level tasks. Not for per-row dispatch.

---

## Next Steps

- [Compute Architecture](../design/compute.md) — Driver, scheduler, sink, quarantine
- [Example scripts](https://github.com/DeepLink-org/Persisting/tree/main/examples/compute)
