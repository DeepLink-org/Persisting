# pPilot

**Durable Run Orchestrator and analysis CLI.**

pPilot is a first-class Persisting component alongside pVisor and pChronicle:

- pPilot plans, schedules, resumes, and reconciles many Runs;
- pVisor owns execution and the lifecycle of each Run/Attempt;
- pChronicle owns canonical Run history and derived views.

pPilot consumes Run contracts and results and is the user-facing entry point
for querying trajectory data. It does not own provider protocol adaptation,
execution drivers, or trajectory storage formats; those query implementations
remain in pChronicle. It does expose the pPilot-side `AgentAbiClient` used for
heartbeat, process registration, checkpoint quiescence, and effect journaling
with the pVisor that owns a Run.

The client discovers the Run-scoped Unix endpoint from pVisor-injected
environment values. The compact wire types are owned by pVisor alongside the
Unix transport implementation.

```bash
# The public binary is feature-gated so library-only builds stay lightweight.
cargo build -p persisting-ppilot --features cli --bin ppilot

ppilot run plan.py --workers 8 --sink ./results
ppilot run plan.py --workers 8 --sink ./results \
  --control-uri s3://my-bucket/ppilot-control
ppilot self-test

ppilot chronicle import ./trajectories.ndjson ./storyline-store
ppilot convert ./openai-data ./storyline-store --to lance
ppilot convert ./storyline-store ./recovered --from lance --to openai_msg
ppilot convert ./task.actf.json ./actf-store --to lance
ppilot convert ./actf-store ./recovered-actf --from lance --to actf
ppilot query point ./storyline-store --session-id run-001 --step-id 7
ppilot query point ./storyline-store --session-id run-001
ppilot query batch ./storyline-store --session-id run-001,run-002 --step-id 7
ppilot query follow ./capture --agent-id agent-001 --session-id run-001
ppilot query sql ./storyline-store \
  --sql "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source"
ppilot query sql s3://trajectory-bucket/persisting/storylines \
  --sql "SELECT COUNT(*) AS runs FROM runs"
ppilot query sql ./trajectories.ndjson --sql-file analysis.sql
ppilot query sql ./openai-data \
  --sql "SELECT _file_, COUNT(*) FROM steps WHERE _file_ LIKE 'cybergym_%' GROUP BY _file_"
ppilot query sql ./actf-data --source actf \
  --sql "SELECT session_id, _file_ FROM runs WHERE _file_ LIKE 'bench/%'"
ppilot query sql ./openai-data --max-files 200000 --max-entries 400000 \
  --max-file-bytes 67108864 \
  --max-concurrent-files 4 --cache-bytes 536870912 \
  --memory-limit-bytes 2147483648 --spill-path /var/tmp/ppilot \
  --max-spill-bytes 10737418240 --timeout-seconds 600 \
  --max-output-rows 10000000 --query-metrics \
  --sql "SELECT COUNT(*) FROM runs"
ppilot query sql ./storyline-store \
  --table labels=csv:./labels.csv \
  --table metadata=json:./metadata.json \
  --sql 'SELECT r.session_id, l.score, m.category FROM runs r JOIN labels l USING (session_id) JOIN metadata m USING (session_id)'

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
When durable control is enabled, pVisor also publishes an epoch-fenced
pChronicle Attempt record with heartbeat expiry and the complete terminal
`RunResult`. After a pPilot restart, a live record is deferred instead of being
re-dispatched, and an uncommitted terminal record is recovered into RunCommit
and the user sink. A lease that has not yet produced an Attempt record is also
deferred until its TTL proves it orphaned.

`ppilot chronicle import` validates ATIF, ACTF, OpenAI-message JSON, or a directory and
atomically replaces the corresponding Storylines in a local or object-store Lance store.
`ppilot convert` is the dedicated conversion entry point for ATIF, ACTF, OpenAI messages,
Storyline JSON, AgenticMD, and the three-table Lance representation. Document outputs are
directories; losslessly imported OpenAI corpora recover their original file grouping.
`ppilot query` exposes `sql`, `point`, `batch`, and `follow` modes backed by
pChronicle and writes JSONL to stdout. Point and batch operate on normalized
Storyline Lance data; follow continuously reads committed canonical events.
Batch Storyline reads use one snapshot across `runs`, `steps`, and
`tool_calls`, rather than N point lookups. SQL registers the same three tables
for Storyline Lance and ATIF inputs. `s3://` inputs are automatically recognized
as Lance; credentials come from the AWS provider chain. Use `--sql-file -` to
read SQL from stdin. Repeat `--table NAME=FORMAT:PATH` to register external
CSV (`csv`), JSON array (`json`), or newline-delimited JSON (`jsonl`/`ndjson`)
tables in the same DataFusion session before executing the query.
The former `ppilot query <INPUT> --sql ...` spelling remains compatible.

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
