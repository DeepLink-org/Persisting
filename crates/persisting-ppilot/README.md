# pPilot

**Durable Run Orchestrator and analysis CLI.**

pPilot is a first-class Persisting component alongside pVisor and pChronicle:

- pPilot plans, schedules, resumes, and reconciles many Runs;
- pVisor owns execution and the lifecycle of each Run/Attempt;
- pChronicle owns canonical Run history and derived views.

pPilot consumes Run contracts and results and is the user-facing entry point
for querying trajectory data. It does not own provider protocol adaptation,
execution drivers, or trajectory storage formats; those query implementations
remain in pChronicle. It does expose the pPilot-side `AgentAbiClient` used to
negotiate heartbeat, process registration, checkpoint quiescence, and effect
journaling with the pVisor that owns a Run.

The client discovers the Run-scoped Unix endpoint from pVisor-injected
environment values. Wire types are defined in `persisting-proto::agent_abi`,
so the same client semantics can later run over virtio-vsock.

```bash
# The public binary is feature-gated so library-only builds stay lightweight.
cargo build -p persisting-ppilot --features cli --bin ppilot

ppilot run plan.py --workers 8 --sink ./results
ppilot run plan.py --workers 8 --sink ./results \
  --control-uri s3://my-bucket/ppilot-control
ppilot self-test

ppilot query ./storyline-store \
  --sql "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source"
ppilot query s3://trajectory-bucket/persisting/storylines \
  --sql "SELECT COUNT(*) AS runs FROM runs"
ppilot query ./trajectories.ndjson --source atif --sql-file analysis.sql

# Scenario 1: bounded parallel trajectory production through pVisor.
ppilot produce production.py --output ./runs --parallelism 8 \
  --cluster-network-limit 10mbps

# Scenario 2: pChronicle SQL over automatically balanced ATIF shards.
ppilot analysis ./atif --output ./analysis --parallelism 8 \
  --sql 'SELECT session_id, agent_id FROM runs ORDER BY session_id'

ppilot process ./atif --script metrics.py --mappers 8 --output ./processed
```

With `--sink`, pPilot issues a monotonically increasing lease epoch before
dispatch, carries it through Worker and pVisor, and commits the terminal
`(run_id, attempt_id, lease_epoch, result digest)` with pChronicle CAS. The
durable result journal is written before the CAS and the user sink afterwards;
startup reconciliation repairs either crash window. An unexpired lease held by
another owner is not stolen. Once its pVisor attempt is proven absent, the
reconciler authorizes an explicit takeover with a newer fencing epoch.
Long-running attempts renew the same epoch in the background until their
terminal result reaches RunCommit; loss of ownership cancels the local attempt.

`ppilot query` registers the same `runs`, `steps`, and `tool_calls` tables for
Storyline Lance and ATIF inputs. It accepts one read-only SQL statement and
writes JSONL rows to stdout. `s3://` inputs are automatically recognized as
Lance; credentials come from the AWS provider chain. Use `--sql-file -` to
read SQL from stdin.

## pPilot ↔ pVisor runtime control

Normal `run` and `produce` execution automatically starts a job-scoped,
in-process Supervisor; there is no service to deploy or separate command to
run. pPilot injects a versioned endpoint, controller epoch, and one-time token
into each `RunSpec`. pVisor registers before preparing its runtime, receives
initial quota directives, and then keeps a heartbeat/command connection alive
for the Attempt. A Supervisor connection failure is advisory: pVisor records a
warning and continues standalone, and a later disconnect never cancels a Run.

On `produce`, `--cluster-network-limit RATE` divides the job rate into fixed
shares using the requested maximum parallelism and sends one share to each
pVisor. pVisor applies its grant to traffic intercepted by that Run's
OverlayNet proxy. This conservative first version keeps the aggregate below the
configured rate but does not dynamically lend idle shares. The plan executor
used by `ppilot run` does not expose this option because its long-lived Python
host is not a per-Run proxy client. This is not an organization-wide
distributed token ledger, and direct sockets can still bypass the explicit
proxy. Runtime directives are authenticated and fenced by controller epoch,
Run lease epoch, and monotonic directive sequence; the first live directive
implemented is `Cancel`.

Every pPilot task is a child `RunSpec` with a stable `task_id`, a logical
`parent_run_id`, and `ppilot.job_id` orchestration metadata. While the task is
running, `PilotRuntimeBridge` keeps the pVisor Agent ABI session alive, handles
shutdown directives, registers the worker process, and journals the task as a
semantic effect with a stable idempotency key.

For a live logical checkpoint, pVisor first publishes `Quiesce`. pPilot stops
admitting new effects, waits until the current opaque Python `execute(item)`
reaches its terminal safe point, completes its effect, and acknowledges the
checkpoint. pVisor then snapshots OverlayFS and publishes `Continue`. Arbitrary
Python code is not preemptively snapshotted; checkpoint latency therefore
includes the remaining duration of the active task.

## Batch trajectory production

`produce` takes an executable Python planner. `plan()` may be a synchronous or
asynchronous iterator and emits complete Run descriptions incrementally:

```python
# production.py
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("--count", type=int, default=10)
args = parser.parse_args()

def plan():
    for index in range(args.count):
        yield {
            "id": f"math-{index:04d}",
            "agent": "codex",
            "command": ["codex", "exec", f"Solve task {index}"],
            "cwd": "/work/eval",
            "env": {"DATASET_ITEM": str(index)},
        }
```

```bash
ppilot produce production.py \
  --output ./trajectory-runs \
  --parallelism 16 \
  --batch-id nightly-2026-08-02 \
  -- --count 1000
```

The planner runs in the interpreter selected by `--python` (or
`PERSISTING_PYTHON`), isolated from the Rust process. Arguments after `--` are
forwarded through `sys.argv`. Each yielded object requires a path-safe unique
`id` and non-empty `command`; `agent` defaults to `agent`, while `cwd` and
string-valued `env` are optional. pPilot validates and dispatches items as they
arrive with bounded backpressure instead of materializing the full plan.

Each item gets an independent pVisor workspace containing `run.json`, capture
state, and `run-bundle.json`. The Bundle records `parent_run_id`, `task_id`,
`ppilot.batch_id`, and `ppilot.scope`. The batch id defaults to the planner file
stem. `production-report.json` summarizes all terminal Runs. Existing per-Run
workspaces are refused rather than overwritten. Version-1 `.json` manifests
remain readable as a compatibility input, but Python planners are the primary
interface.

## Batch trajectory analysis

ATIF may be a JSON object, array, JSONL/NDJSON file, or a directory of these
files. pPilot asks pChronicle to validate and normalize all documents, sorts by
effective session id, and creates `min(trajectory_count, parallelism)` balanced
round-robin shards.

```bash
ppilot analysis ./atif --fmt json --sql-file analysis.sql

ppilot analysis ./atif \
  --output ./analysis \
  --parallelism 8 \
  --sql-file analysis.sql
```

Without `--output`, only the combined result is emitted to stdout. The default
format is JSONL; `--fmt json` emits one JSON array and `--fmt toml` emits
`[[rows]]` tables. With `--output`, stdout stays empty and pPilot writes
`part-*.jsonl`, `results.<fmt>`, and `analysis-report.json`. TOML cannot encode
SQL null values, which is reported as an explicit error.

Analysis outputs:

- `part-00000.jsonl` …: one SQL result per shard;
- `results.jsonl`: deterministic concatenation in shard order;
- `analysis-report.json`: shard membership, row counts, and output paths.

The SQL is executed independently on each shard. Row projections and
per-trajectory/grouped analysis concatenate naturally. A global aggregate such
as `SELECT COUNT(*) FROM runs` produces one partial row per shard; reduce those
partial rows downstream, or use the unsharded `ppilot query` command when one
globally aggregated result is required.

## Distributed trajectory processing

`process` is reserved for typed processing and globally correct reductions:

```bash
ppilot process ./atif --script metrics.py --mappers 16 --output ./processed

ppilot process ./atif \
  --output ./count-analysis \
  --mappers 16 \
  --count steps
```

Python jobs define `map(records, context)` and `reduce(partials, context)`
(`mapper`/`reducer` and one-argument functions are also accepted). Rank 0 reads
the script and input, then transfers both script bytes and deterministic ATIF
shards through Pulsing. Remote ranks therefore do not need a shared filesystem.
The reducer runs on the driver in shard order. Without `--output`, its JSON value
is printed to stdout; with an output directory, pPilot writes `results.json` and
`process-report.json`, including the script SHA-256 and mapper provenance.

The five `--count` values remain built-in processors for common metrics:

`--count runs|steps|tool-calls|llm-calls|copied-context-steps` exposes five
typed federated Agent-analysis metrics. pPilot
sends deterministic ATIF shards to named Pulsing analysis workers; every worker
normalizes its shard with pChronicle and returns a partial count. Rank 0 checks
that every shard returned exactly once and writes the checked sum as the single
row in `results.jsonl`. Without torchrun, the same protocol runs on an
in-process Pulsing fleet. Under torchrun, every rank hosts one analysis worker;
only rank 0 reads the input and writes the report.

`llm-calls` sums `steps.llm_call_count`; `copied-context-steps` counts only rows
marked as copied context. This version intentionally does not rewrite arbitrary
SQL. The typed aggregate boundary prevents joins, distinct counts, averages,
and other non-trivial plans from being merged with incorrect semantics.
