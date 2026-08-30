# Storyline three-table Lance storage

`StorylineLanceStore` is pChronicle's Storyline-native normalized storage
representation. It sits beside the raw `events.lance` event log and does
not replace it.

The logical wire schema follows
[RFC-0001 § Wire schema](../../rfcs/0001-storyline-format.md#wire-schema).
Field-by-field conversions for ACTF, ATIF, and OpenAI Messages follow the
mapping sections of
[RFC-0004](../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping),
[RFC-0008](../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping),
and
[RFC-0009](../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping).
This design defines only the Lance physical projection of Storyline.

## Projection contract and closed loop

Storyline retains the Hub interchange contract (path A), while the
three-table store is a rebuildable silver projection of canonical
`events.lance` (path B). The two uses share a schema, but not write
identity: interchange imports and direct `replace_storyline` calls carry
no canonical lineage. Only the events projector may publish a `CURRENT`
with projection lineage.

```text
events.lance (source of truth)
  ├─ serve startup/runtime ─► runs + steps + tool_calls + objects
  ├─ append-compatible sync ─► replace only sessions touched by the append suffix
  └─ Catalog fallback ──────► project a pinned events snapshot when missing or stale
```

`CURRENT` pins exact Lance versions for all four tables and records the
source URI and source identity, `fact_version`, `fact_rows`, the source
layout revision at build time, projector and recipe identity, recipe hash,
and completeness. `fact_version` and `fact_rows` are the freshness
watermark. Compaction changes only the layout revision and does not stale
a projection. Direct document writes clear lineage; maintenance preserves
it.

Incremental sync treats `[previous_fact_rows, fact_rows)` as an append
range only because the canonical manifest validates
`fact_rows == total_rows()`. Layout maintenance must preserve both
replacement row count and segment order, so compaction cannot move that
logical watermark. After reading the range, the projector also requires
the returned record count to equal the exact range length; violating any
of these proof obligations fails closed instead of silently skipping
facts.

Operational commands:

```bash
pchronicle serve --control 127.0.0.1:0 ./trajectory-data
pchronicle status ./trajectory-data --format json
```

Before readiness, `serve` discovers every validated non-empty canonical
Store and converges its deterministic sibling `storyline`. At runtime it
discovers new Stores, performs append-compatible sync or full rebuild as
required, and retries bounded failures without blocking durable canonical
writes. A destination without matching lineage is foreign and is never
overwritten. `status` reports `fresh`, `stale`, `missing`, or `error`
plus the source watermark and selected generation.

Catalog merges a lineage-linked sidecar with the events source into one
logical source. When `sources.projection_status` is `fresh`, normalized
queries use the three tables. When it is `stale`, Catalog hides the
sidecar and falls back to a deterministic projection of the pinned events
snapshot. `projection_generation` exposes the generation actually
selected. A Storyline document store without lineage is never inferred to
be a projection of canonical events.

The Gateway-backed Warehouse has an explicit live-read path for point
trace observation: after the Catalog resolves an already discovered
canonical source, `/api/events`, `/api/storyline`, and
`/api/trajectory-view` reopen its latest visible events manifest. This
does not change the immutable snapshot semantics of broad SQL queries,
and it does not make the derived Storyline sidecar authoritative.

The projection supervisor is part of `serve`, applies bounded concurrency
and retry, and shuts down with the process. It remains outside the
Gateway capture write path, so projection or Catalog refresh failures
cannot block canonical event writes.

This page owns the three-table physical schema, the content layer,
Snapshot publication, query integration, and maintenance semantics. Fact
source and projection ownership are in
[Run storage](trajectory-storage.md). The user query workflow is in
[Discover and query](../guides/discover-and-query.md).

This is pChronicle's only normalized three-table model. The older ATIF
`sessions` / `steps` / `tool_calls` layout, `NormalizedStore`, and
in-memory joined views have been removed. ATIF still exists as an
import/export format, but a query first converts it to Storyline and then
projects it onto the `runs` / `steps` / `tool_calls` schema defined here.
A second table structure is not maintained.

## Table model

| Table | Grain | Logical primary key | Foreign key |
|---|---|---|---|
| `runs.lance` | one row per Storyline | `document_id` | — |
| `steps.lance` | one row per turn | (`document_id`, `step_id`) | `document_id` → runs |
| `tool_calls.lance` | one row per tool call | (`document_id`, `step_id`, `call_index`) | (`document_id`, `step_id`) → steps |

`run_id` is a Run grouping key. One Run can contain a main Story and
several subagent Stories, so several rows in `runs.lance` may share the
same `run_id`. The internal `document_id` uses an explicit
`trajectory_id` and falls back to `session_id`. It is the document-scope
key for three-table mutation.

`steps.message` is stored as `message_kind` plus `message_value`. The
kind is a fixed enum for `null`, `text`, `parts`, or `json`. The value
holds the normalized JSON raw value and remains protected by the large-
object offload path. `reasoning_effort` is split the same way into
`reasoning_effort_kind` and `reasoning_effort_value`; the kind is a
fixed enum that distinguishes string, number, and JSON escape. Larger
JSON values such as `arguments`, `observation`, and `result` stay as
UTF-8 JSON columns and remain under the content/offload layer. Identity,
order, type, time, and performance fields use independent Arrow scalar
columns so they can be filtered and analyzed.

`runs.schema_version` and `runs.origin` store the strict Storyline wire
version and origin identity. `runs.task` and `runs.prompt` store
document-level `/task` and `/prompt`. `runs.started_at` and
`runs.finished_at` are UTC nanosecond Timestamp columns. `runs.extra`
and `runs.meta` store document-level `/extra` and `/meta`.
`steps.finished_at` is also a UTC Timestamp column. `steps.env` and
`steps.prompt` store turn env and turn `/prompt`. `tool_calls.kind` and
`tool_calls.response` store the tool-event type and `response`.
`agent_extra`, `final_metrics`, `extra`, `meta`, `unknown_fields`,
`metrics`, and `response` use the Lance `lance.json` extension type
(JSONB, physically `LargeBinary`). Lance/DataFusion expose them as Arrow
JSON strings on read and support JSON path functions and predicate
pushdown. Offload does not replace a whole JSON cell. It walks
objects/arrays recursively and replaces values above the threshold with a
content descriptor. The outer envelope remains queryable with
`json_get_*`; a full read restores recursively. `message_value`,
`observation`, `arguments`, `result`, `results`, and other potentially
large fields continue to use the content/offload layer.
`steps.turn_ordinal` is the authoritative turn-array order; `step_id` is
identity only and does not participate in reordering. `had_tool_calls`
keeps an explicit empty array distinguishable from a missing field.
Missing columns in older tables decode as field absence.
`message_kind`/`message_value` and
`reasoning_effort_kind`/`reasoning_effort_value` exist together as grouped
schema fields. These objects are not split into independent SQL columns.

`steps.timestamp` is a UTC-normalized `Timestamp(Nanosecond, "UTC")`
query column. The writer rejects a non-null time that is invalid, out of
range, or not exactly representable as nanoseconds. SQL sort, range
filters, and time aggregation use `timestamp` directly. Storyline
reconstruction uses the normalized UTC time. The reader still accepts the
older `Timestamp(Millisecond, "UTC")` layout.

`steps.latency`, `steps.ttft`, and `tool_calls.duration` are nullable
`BIGINT` columns in milliseconds. The unit comes from field semantics and
query-field documentation; it is no longer encoded in a column-name
suffix.

`steps.observation` stores the complete, authoritative arbitrary JSON
observation. `had_observation` stores presence semantics.
`tool_calls.results` is only a query column derived from associable
`observation.results[]` items. It does not rebuild observation in
reverse. A read fails closed if the derived column disagrees with the
authoritative observation. Turn ordinals and call indexes must also be
unique and contiguous from zero.

## Large content layer

### Goals and bounds

Long reasoning, tool output, source code, logs, and multimodal payloads
in Agent trajectories produce a few oversized cells in columnar tables.
Inlining them next to identity, order, type, and metrics forces ordinary
filters and aggregations to pay for larger fragments, page cache, and
decode cost. pChronicle therefore adds a shared content layer beside the
three tables, under three constraints:

1. Inside one schema version, the Arrow schema and SQL results of
   `runs` / `steps` / `tool_calls` stay stable. Schema changes are
   published explicitly through `CURRENT.schema_version`. The content
   layer is an internal physical optimization.
2. Small values stay inline. Only UTF-8/JSON cells that reach the
   threshold are offloaded, so every read does not degrade into a KV
   lookup.
3. Content is addressed by raw bytes and reused across Storylines. The
   content layer does not mix in trajectory primary keys, lifecycle, or
   business-level deduplication.

The current implementation does not invent a custom Lance file format or
a private index type. It composes Lance Blob v2, ordinary BTree scalar
indexes, and a DataFusion execution node. That gives the needed lazy
materialization while keeping the maintenance surface inside
pChronicle's own protocol and execution plan.

### Internal descriptor protocol

Content columns above the default 64 KiB are temporarily encoded in the
three tables as:

```text
<RS>PCHRONICLE-CONTENT:<type>:<codec>:<blake3-256>:<raw_length>:<preview-base64url>
```

| Field | Current encoding | Role |
|---|---|---|
| magic | `PCHRONICLE-CONTENT` | strict recognition of an internal reference |
| logical type | `u` / `j` / `b` | UTF-8, JSON; the binary tag is reserved for later binary columns |
| codec | `i` / `z` | identity or Zstd |
| content id | 64-digit hex BLAKE3-256 | address, checksum, and cross-trajectory reuse of uncompressed raw bytes |
| raw length | `u64` | post-decompress length check, and cost judgment without the payload |
| preview | URL-safe Base64 | a safe prefix of at most 256 UTF-8 bytes by default |

Descriptors may exist only in internal physical columns. User text that
happens to start with the magic is forced offload and restored on read,
so a user string cannot be mistaken for a reference. Public read, SQL,
conversion, and export APIs must return the full value or an explicit
preview. They must not leak the descriptor.

`objects.lance` uses these physical columns:

| Column | Role |
|---|---|
| `content_id` | BLAKE3 content address; has a BTree index |
| `logical_type`, `media_type` | logical type and MIME hint |
| `raw_length`, `stored_length`, `codec` | integrity check and storage cost |
| `preview` | safe preview with no Blob I/O |
| `payload` | Lance Blob v2 holding identity/Zstd bytes |
| `created_at_ms` | object creation time |

### Write, reuse, and publication

The writer handles candidate cells in this order:

```text
原始 UTF-8/JSON
  ├─ 小于阈值 ───────────────────────────────► 原值内联
  └─ 达到阈值 / 命中 magic
       ├─ BLAKE3(raw bytes) + UTF-8 preview
       ├─ Zstd；没有净收益则保留 identity
       ├─ batch 内按 content_id 合并并检查碰撞
       ├─ BTree 批量查询 objects.lance，跳过已存在对象
       └─ 先提交对象 version，再写三表 descriptor，最后发布 CURRENT
```

Objects must be durable before the reference. `CURRENT` pins the exact
Lance versions of the three business tables and the object table
together. Failure at any step does not publish a new Snapshot:
unreachable objects may remain, but dangling references and half-commits
across tables are not published. Cross-trajectory reuse depends only on
the content address, not on session lifetime, so the same long text is
stored once across Runs. A write is rejected if the same content id
appears in one batch with disagreeing codec, raw length, or stored bytes.

The object layer stays append-only during ordinary writes. GC is not on
the write hot path. Explicit `maintain` scans only the three tables'
content-reference columns, computes the reachable content ids of the
current Snapshot, and drops unreachable payloads. Production still needs
metrics for object growth rate, unreachable bytes, and maintenance time.

### Query-time lazy materialization

`StorylineDataSource` lets Lance finish business-table projection, safe
predicates, scalar indexes, limit, and parallel scan first, then inserts
`ContentHydrationExec` into the plan:

- if the query does not reference a content column, `objects.lance`
  payloads are not opened;
- only content ids that actually appear in the projection are collected,
  then looked up through BTree in groups of at most 512;
- Blobs are read in batches by row address, decompressed, checked for
  length and BLAKE3, then restored as the original Utf8 column;
- content-column predicates must not run against the descriptor; they
  stay after hydration for DataFusion to evaluate;
- `Preview` mode returns only the UTF-8 prefix from the descriptor, does
  zero payload I/O, and rejects content-column predicates so a preview
  cannot be mistaken for the full value.

Large-content cost is therefore paid only by queries that actually read
that content. Identity filters, counts, grouping, and metric analysis
keep the compact three-table columnar path.

## Commit layout

```text
root/
├── CURRENT
├── objects.lance/
└── generations/
    └── <table-generation>/
        ├── runs.lance/
        ├── steps.lance/
        └── tool_calls.lance/
```

The first import creates the three normalized Lance datasets, the shared
`objects.lance`, and the scalar indexes. Later `replace_storyline` no
longer reads or rewrites the whole store. It merge-upserts by each
table's primary key and deletes only old keys that no longer exist under
the specified `document_id`. Each replace produces a new logical
snapshot. `CURRENT` is JSON that records the required store
`schema_version: 1`, the logical snapshot id, the physical
`table_generation`, and the exact Lance version id of each of the three
tables plus the object table. Objects are persisted first, then the
three business tables, and `CURRENT` is updated last. Failure can leave
unreachable objects at worst; it does not publish dangling references or
a half-commit across tables.

Threshold, preview length, and Zstd level are configurable through
`StorylineContentOptions`. The current three-table schema is fixed at
version 1.

Older Lance MVCC versions are retained by default so an already-open
reader can pin a Snapshot and recover from failure. Frequent incremental
updates accumulate fragments, delete files, and unmerged index deltas.
Ordinary replace does not refresh indexes or compact, so one write
request does not grow a maintenance tail. Production uses `maintain` to
compact the three tables in parallel, fill/refresh indexes, GC content,
and vacuum by retention window. The four dataset versions produced by
maintenance still update `CURRENT` atomically first, then reclaim older
versions and expired non-current physical generations. `CURRENT` must be
a JSON pointer that contains the schema version and every exact version.
A missing or unknown schema version fails closed before any Lance table
is opened, and the old plain-text generation pointer is not read.

Local writes are serialized by an in-process lock and a file lock. Object
stores use optimistic CAS through a conditional ETag/version update of
`CURRENT`. A stale commit cannot move `CURRENT`. After a CAS conflict,
`StorylineLanceStore` returns an error; it does not re-read, merge, or
retry automatically. A caller that chooses to retry must start a complete
replace from the latest snapshot. An upper lease can reduce conflicts; it
does not change this failure semantic.

## Rust API

```rust
let store = StorylineLanceStore::open(path).await?;
store.replace_storyline(&storyline).await?;
let restored = store.get_storyline_full("session-id").await?;
let report = store.maintain(&LanceMaintenanceOptions::default()).await?;
```

`replace_storyline` replaces the related rows in the three tables at
`document_id` granularity and keeps the other Storylines in the same
store.

`get_storyline_full` means it will read the three tables and restore the
full content of that Storyline. Store-local paging APIs that were unused
by CLI or Web have been removed. Product-level listing, paging, and
projection are owned by Catalog, the Warehouse API, and DataFusion query,
so a second unreachable read protocol is not maintained.

First import and replace both write the three tables in parallel. Arrow
rows are lazily encoded in batches of at most 8192 and streamed into
Lance, so a large import does not keep a full-table Arrow copy.
`CURRENT` is parsed once. The DataSource then opens each table directly
at the pointed-to version instead of validating and reopening the same
dataset.

Production maintenance goes through the `StorylineLanceStore::maintain`
Rust API. The public CLI does not expose a maintenance command.

## DataFusion datasource

`StorylineDataSource` pins the three business-table versions and the
object-table version from `CURRENT` at open time, and registers the three
datasets as `runs`, `steps`, and `tool_calls`. Even if the writer later
switches `CURRENT`, an already-open query keeps the same three-table
Snapshot.

```rust
let source = StorylineDataSource::open(path).await?;
let ctx = source.session_context()?;
let rows = ctx
    .sql("SELECT step_id, source FROM steps WHERE session_id = 's-1' ORDER BY step_id")
    .await?
    .collect()
    .await?;
```

The datasource uses Lance's native DataFusion execution plan. It supports
column pruning, predicate and limit pushdown, and an unordered physical
scan that allows parallel reads. Queries that need order must write
`ORDER BY step_id, call_index` explicitly. Queries that do not reference
large content columns do not open Blobs. When a content column is
referenced, restoration happens in batches after Lance projection, safe
predicates, and limit. Predicates on content columns are not pushed down
onto the internal reference; DataFusion evaluates them after restoration
so SQL semantics stay the same. Internal references are never returned by
pChronicle read, query, or export APIs. Scanning the underlying Lance
files while bypassing pChronicle is a diagnostic interface and is outside
that guarantee.

A preview UI can set `StorylineDataSourceOptions::content_read_mode` to
`StorylineContentReadMode::Preview`. That mode returns a short UTF-8-safe
preview directly from the descriptor with zero Blob payload I/O. Content-
column predicates are rejected in preview mode so a preview cannot be
treated as the full value.

These scalar indexes are built when a table generation is first created:

| Table | BTree | Bitmap |
|---|---|---|
| runs | `document_id`, `session_id`, `run_id` | — |
| steps | `document_id`, `session_id`, `timestamp` | `effective_kind`, `source` |
| tool_calls | `document_id`, `session_id`, `tool_call_id` | `function_name` |

The indexes target Story/Run location, tool-call lookup, and type
filters. `step_id` recounts from small values inside each Storyline and
has low global selectivity, so it does not get its own BTree. A combined
predicate first locates one Storyline with `session_id`, then filters a
short step range. DataFusion index predicates are pushed down as Lance
`ScalarIndexQuery`.

`StorylineDataSourceOptions` can set `use_scalar_indexes` and
`scan_in_order` explicitly. The default enables indexes for online
analytic queries and turns physical order off. Turning indexes off is
mainly for benchmarks, diagnosis, or a full-scan control on a tiny table.

## Unified query engine

`ChronicleQueryEngine` is the public read-only SQL facade. All six disk
formats (Canonical Event, Storyline Lance, AgenticMD, ATIF, OpenAI Msg,
ACTF) open through the single entry
`ChronicleQueryEngine::open(format, path, options)` and register the
semantically matching query tables, so SQL does not change with the
physical format:

```rust
use persisting_pchronicle::query::{ChronicleQueryEngine, ChronicleQueryExecutionOptions};
use persisting_pchronicle::document::DocumentFormat;

let engine = ChronicleQueryEngine::open(
    DocumentFormat::Storyline,
    "./storyline-store",
    ChronicleQueryExecutionOptions::default(),
).await?;
let batches = engine.query(
    "SELECT session_id, step_id, source FROM steps WHERE step_id >= 10"
).await?;

let atif = ChronicleQueryEngine::open(
    DocumentFormat::Atif,
    "./trajectories.ndjson",
    ChronicleQueryExecutionOptions::default(),
).await?;
let jsonl = atif.query_jsonl(
    "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"
).await?;
```

`DocumentFormat::CanonicalEvent` registers the `events` table;
`runs`/`steps`/`tool_calls` are not registered live by default —
Storyline query surfaces prefer the lineage-fresh Storyline Lance
projection, and without one a bounded row/byte-budget fallback runs
(budget exhaustion is an explicit error, never a silent truncation). The
other five formats register `runs`/`steps`/`tool_calls`.

`query` returns Arrow `RecordBatch` values, which suits further
server-side processing. `dataframe` returns a lazy DataFrame for extra
DataFusion transforms or plan inspection. `query_jsonl` is for CLI/API
boundaries. Callers can also take the `SessionContext` from `context()`
to register UDFs or extra tables. `backend_info()` returns a
`QueryBackendInfo` that reports `format` / `tables` / `capabilities` /
`snapshot` per the provider's real implementation; filter pushdown
capability distinguishes `Unsupported` / `Inexact` / `Exact` /
`ExpressionDependent` and is never overstated.

The unified document source entry `open_document(format, path)` accepts a
single ATIF JSON object, a JSON array, JSONL/NDJSON with one complete
trajectory per line, and directories containing such ATIF documents. File
paths register as per-file lazy `StreamingTable`s by default: the
manifest freezes paths and file identities at open time and scans read
only the hit files; directories are discovered in stable order, each file
is an independent partition, and fixed-size Arrow batches provide
backpressure.

### pChronicle + JSON projection query fast path

The old path fully parsed each JSON document into a format object, then
ran `ATIF → Storyline → three-table rows → full-width Arrow` before SQL.
Even a query that only needed `source` or `COUNT(*)` constructed unused
large fields such as message, reasoning, metrics, and tool calls. The new
path moves the optimization boundary forward to `TableProvider::scan`:

```text
SQL / DataFrame
  → DataFusion projection + filters
  → FileScanSpec
      ├─ _file_ = / IN / LIKE：manifest 文件裁剪
      ├─ session_id：trajectory 裁剪
      ├─ step_id / source：step 裁剪
      └─ projected column set
  → BufRead / serde streaming decoder
      └─ DeserializeSeed + Visitor + IgnoredAny
  → 只为命中行解码被引用字段
  → projected Arrow RecordBatch
  → DataFusion 保留 inexact filter 再次校验
```

The current fast path covers ATIF single objects, arrays (including
pretty JSON), and JSONL/NDJSON, plus ACTF single objects and arrays. The
target table is `steps`, and the physical plan must have strict column
pruning. The path is intentionally conservative:

| Input/query | Execution path |
|---|---|
| ATIF object/pretty object + projected `steps` | reader-backed seeded projected decoder |
| ATIF array/pretty array + projected `steps` | `fill_buf` structural scan + bounded element buffer + seeded `from_slice` |
| ATIF JSONL/NDJSON + projected `steps` | `BufRead` per record, bounded record buffer |
| ACTF object/array + projected `steps` | reader/slice seeded projected decoder |
| Safe simple predicates on `_file_`, `session_id`, `step_id`, `source` | may prune early; DataFusion still rechecks |
| `SELECT *` | full-normalization fallback |
| `runs` / `tool_calls` | full-normalization fallback |
| OpenAI-message | full-normalization fallback |
| Unproven-safe expressions, OR/functions/cross-column conditions | no pre-prune; DataFusion evaluates |

`DeserializeSeed` passes the query projection and safe predicates into
the `Visitor`. Unreferenced fields go to `IgnoredAny` for a syntactic
scan and do not construct a `Value`/Storyline. ATIF JSONL/NDJSON is read
record-by-record with `BufRead`. The JSON-array structural scanner
recognizes strings and escapes, extracts one trajectory/document without
building a DOM, then runs projected parse through the slice decoder.
Callers can set `max_record_bytes` to bound a single document/record.
There is no default per-record cap; only the `max_file_bytes` file bound
remains. None of the three paths copy the whole file first.
The Arrow encoder also creates only projected columns. `COUNT(*)` uses a
legal zero-column batch. The lightweight path checks JSON, required
fields, duplicate sessions, duplicate steps inside a hit document, and
in-table constraints. Cross-table referential integrity stays with the
import path or the full fallback. That boundary keeps ad-hoc queries from
taking on import semantics without lowering SQL result correctness.

Query metrics additionally report `projected_files`, `streamed_records`,
`streaming_buffer_peak_bytes`, scanned/pruned documents,
scanned/pruned/emitted rows, and `projected_arrow_bytes`, so the four
costs — source-byte scan, input buffer, JSON-field materialization, and
Arrow output — can be distinguished. Repository benchmarks report
median/P95, rows/s, independent-process peak RSS, and allocation
calls/bytes observed by a counting allocator. Those are regression
numbers for a specified corpus, query, and machine, not a cross-
environment SLA.

The path still sequentially scans every JSON byte of a hit file. It is
not an in-file index. One-shot or controlled-batch queries can use JSON
directly. Very large, remote, or repeated queries should convert to Lance
first and use Snapshot, column pruning, parallel fragment scan, and
scalar indexes.

ATIF import also defaults to `AtifReader`. An empty store uses one
producer pass for validation, Storyline normalization, and three-table
split, then creates the three Lance datasets in parallel over three
bounded Arrow channels. An existing store replace in incremental batches
of at most 256 Storylines. Both paths switch `CURRENT` atomically once,
only after every input and three-table write succeeds.

The CLI uses the same engine and emits stable JSONL:

```bash
pchronicle query ./trajectories.ndjson \
  --sql 'SELECT source, COUNT(*) AS steps FROM dataset.steps GROUP BY source ORDER BY source'

# 含 CURRENT 的三表 store 根目录会被 auto 识别为 Lance
pchronicle query ./storyline-store \
  --sql 'SELECT step_id, source FROM dataset.steps WHERE session_id = '\''s-1'\'' ORDER BY step_id'

# OpenAI/ACTF 目录直接查询；_file_ 为查询期相对路径列，不写入 Lance
pchronicle query ./openai-data \
  --sql "SELECT _file_, COUNT(*) FROM dataset.steps WHERE _file_ LIKE 'batch/%' GROUP BY _file_"
```

Queries are read-only. SQL may use SELECT, CTE, JOIN, aggregation, and
built-in DataFusion functions, but this facade does not run DDL/DML. When
the Lance engine opens, it pins the three versions pointed to by
`CURRENT`, so the three tables in one query session come from the same
Snapshot.

The repository uses a unified Criterion.rs + hyperfine benchmark runner.
Criterion owns CPU-bound conversion, events→Storyline, and three-table
split/reconstruct microbenchmarks. Canonical event append, projection
build/sync/verify, Lance/DataFusion lifetime, JSON streaming, and RSS
scenarios are repeated by hyperfine in independent processes and then
folded into unified JSON, Markdown, and HTML:

```bash
# PR/local smoke workload
just benchmark-pchronicle

# larger nightly workload
just benchmark-pchronicle nightly target/pchronicle-benchmark/nightly

# compare two raw reports produced on the same testbed
just benchmark-pchronicle-compare \
  target/pchronicle-benchmark/main/raw-report.json \
  target/pchronicle-benchmark/current/raw-report.json
```

`raw-report.json` stores raw metrics and environment at
`$["measurements"]...` JSONPath addresses. `bencher.json` is the flat
projection used by the historical platform. `report.md` is written to the
GitHub Actions Job Summary. `report.html` and Criterion detail become
artifacts.

JSON comparisons use a single NDJSON file to avoid the open cost of many
tiny files. A direct ATIF `steps` query passes the DataFusion projection
and the safely pre-prunable `session_id`, `step_id`, and `source`
predicates to the projected decoder. Unreferenced JSON fields are scanned
syntactically only; no Storyline/three-table object is constructed, and
the Arrow batch contains only the columns the plan needs. Object, array,
pretty JSON, and JSONL/NDJSON share the streaming projection decoder.
ACTF `steps` uses the matching projected decoder. `SELECT *`, the other
tables, and OpenAI-message still take the full-normalization fallback.
The lightweight path checks JSON, required fields, and in-table
constraints. Cross-table referential integrity is checked by import or
the full fallback. A pre-parsed in-memory JSON control measures query
logic only, so product workflow cost can be separated from pure in-memory
traversal. The benchmark also reports DataSource cold-open-plus-SQL,
`get_storyline_full` point-lookup, and single-Storyline replace latency,
so warm SQL throughput does not hide write amplification on the online
read/write path.

Performance conclusions should not be written as "Lance is always faster
at every scale and query". An explicitly constructed `MemTable` or
pre-parsed in-memory JSON can still be faster on small data. Default ATIF
streaming solves an upper memory bound; it does not provide a physical
index. Lance's main advantages remain a smaller physical footprint,
near-constant datasource open time, and the gains from column pruning,
parallel scan, and selective indexes.

## Related documents

- [Recorded data, views, and versions](../concepts/facts-and-projections.md):
  why Storyline is a projection.
- [pChronicle architecture](architecture.md): publication and read-
  consistency guarantees.
- [Snapshot](catalog.md): how Source discovery and a pinned Snapshot open
  this store.
- [`pchronicle` reference](../reference/cli.md): current public query,
  import/export, and serve commands.
