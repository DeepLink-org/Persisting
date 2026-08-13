# Langfuse → pChronicle backend feasibility probe

The original recorded verdict and evidence are in [REPORT.zh.md](REPORT.zh.md).
The lazy-catalog retest is in
[RETEST-2026-08-12.zh.md](RETEST-2026-08-12.zh.md), with machine-readable
results in
[recorded-results-2026-08-12.json](recorded-results-2026-08-12.json). The
Server source-routing implementation and release measurements are in
[SERVER-ACCELERATION-2026-08-12.zh.md](SERVER-ACCELERATION-2026-08-12.zh.md),
with machine-readable results in
[server-acceleration-results-2026-08-12.json](server-acceleration-results-2026-08-12.json). The
original run remains in
[recorded-results-2026-08-11.json](recorded-results-2026-08-11.json).

This probe evaluates replacing only Langfuse's ClickHouse analytics backend.
Postgres, Redis, and object storage remain unchanged. It is intentionally an
adapter experiment, not a Langfuse public-API change or a pChronicle storage
format change.

Pinned review baselines:

- Langfuse: `d18a59ad663ffc7c04afc61354186c141b3ec0f3` (v4.7.1)
- Persisting: `94531cf903e5abc336de347588fb1858e9d52b6a` plus the local,
  uncommitted catalog/query-engine work present during the review
- ClickHouse: 25.12.x; the recorded run used 25.12.11.4

The pChronicle catalog used here is experimental. TTAS, Queue/Sampler, Search,
and `persisting-dlcapt` are outside this probe.

## Workload

The default deterministic fixture contains two projects and:

- 100,000 trace/span events, including 100 duplicate logical versions
- 10,000 scores
- 2,000 dataset-run items
- 1,000 blob-storage log rows
- metadata, tags, tool names, large I/O values, and 12-digit decimal costs

Trace IDs map to pChronicle Run IDs. Rows without a trace use synthetic Run
IDs. `project_id` maps to `agent_id`; the complete logical row is retained in
the event payload. The default 200 traces plus synthetic Runs create 210 Lance
event datasets.

The executable defines the review-only `LangfuseAnalyticsBackend` contract and
implements append, point/list/aggregate reads, JSON streaming, flush, and
health. Update and delete methods deliberately return unsupported errors. An
implementation that hides those errors behind a new mutable projection engine
would exceed the bounded-integration decision rule.

## Run pChronicle probe

Use an empty temporary directory; the fixture is about 238 MB.

```bash
PROBE_DIR="$(mktemp -d /tmp/langfuse-pchronicle-review.XXXXXX)"
PCHRONICLE_LANGFUSE_WORKDIR="$PROBE_DIR" \
  cargo run --release -p persisting-pchronicle \
  --example langfuse_backend_feasibility
```

The command writes:

- `$PROBE_DIR/logical_rows.jsonl` — shared deterministic input
- `$PROBE_DIR/pchronicle-store/` — pChronicle data
- `$PROBE_DIR/pchronicle-report.json` — measurements and capability failures

The load phase samples visibility after every acknowledged single-row append,
so `visibility_p95_ms` is an observed percentile rather than a final-row spot
check.

Useful scale controls are `PCHRONICLE_LANGFUSE_EVENTS`,
`PCHRONICLE_LANGFUSE_SCORES`, `PCHRONICLE_LANGFUSE_DATASET_RUN_ITEMS`,
`PCHRONICLE_LANGFUSE_BLOB_LOG_ROWS`, `PCHRONICLE_LANGFUSE_TRACES`,
`PCHRONICLE_LANGFUSE_PRELOAD_BATCH_ROWS`, and
`PCHRONICLE_LANGFUSE_LOAD_SECONDS`.

## Run ClickHouse comparison

Start an isolated ClickHouse 25.12 instance. One Docker example is:

```bash
docker run --rm --name langfuse-pchronicle-review \
  -e CLICKHOUSE_PASSWORD=review-only \
  -p 127.0.0.1:18123:8123 \
  clickhouse/clickhouse-server:25.12
```

In another shell, run the same fixture:

```bash
python3 benchmark/langfuse-pchronicle-review/clickhouse_baseline.py \
  --url http://127.0.0.1:18123 \
  --user default \
  --password review-only \
  --fixture "$PROBE_DIR/logical_rows.jsonl" \
  --output "$PROBE_DIR/clickhouse-report.json"
```

The script sends credentials only in ClickHouse authentication headers. It
drops and recreates only the disposable database
`langfuse_pchronicle_review`. Its schema mirrors the relevant Langfuse v4
patterns: `ReplacingMergeTree`, project/time/trace ordering, full/core event
tables with an incremental materialized view, text and skipping indexes,
async inserts with acknowledgement, and separate score/dataset/blob tables.
It samples visibility after every acknowledged 10-row load batch.

## Fault and regression probes

```bash
cargo test -p persisting-pchronicle --test langfuse_backend_faults -- --nocapture
cargo test -p persisting-pchronicle --test production_scale --test query_engine
cargo test -p persisting-pchronicle store::catalog
```

These cover SIGKILL after an acknowledged append, writer fencing, pinned
snapshots during append, independent Run isolation, maintenance/restart, the
read-only query boundary, and catalog refresh behavior.

## Acceptance gates

- No acknowledged loss and no duplicate logical rows
- Visibility P95 ≤ 2 s; update/delete visibility ≤ 5 s
- Point P95 ≤ 500 ms; list/facet P95 ≤ 1 s
- Dashboard, full-text, and export first-byte P95 ≤ 2 s
- No query class more than 5× slower than the ClickHouse baseline
- Catalog cold start ≤ 30 s and RSS ≤ 2 GiB
- Exact project isolation and full Langfuse v4 update/delete/export semantics

Isolation, durability, mutation, or public-semantic failure is an immediate
NO-GO. A localized performance-only miss may be conditional.
