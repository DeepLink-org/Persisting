# Storyline 三表 Lance 存储

`StorylineLanceStore` 是 pChronicle 的 Storyline-native 规范化存储表示。它与
`events.lance` 原始事件日志并列存在，不替代后者。

逻辑 wire schema 以 [RFC-0001 § Wire schema](../../rfcs/0001-storyline-format.md#wire-schema)
为准；ACTF、ATIF 与 OpenAI Messages 的逐字段转换分别以
[RFC-0004](../../rfcs/0004-actf-format.md#actf-storyline-json-pointer-mapping)、
[RFC-0008](../../rfcs/0008-atif-format.md#atif-storyline-json-pointer-mapping)和
[RFC-0009](../../rfcs/0009-openai-messages-format.md#openai-storyline-json-pointer-mapping)
的映射章节为准。本设计只定义 Storyline 的 Lance 物理投影。

## Projection contract and closed loop

Storyline retains the Hub interchange contract (path A), while the three-table store is a
rebuildable silver projection of canonical `events.lance` (path B). The two uses share a schema,
but not write identity: interchange imports and direct `replace_storyline` calls carry no canonical
lineage. Only the events projector may publish a `CURRENT` with projection lineage.

```text
events.lance (source of truth)
  ├─ serve startup/runtime ─► runs + steps + tool_calls + objects
  ├─ append-compatible sync ─► replace only sessions touched by the append suffix
  └─ Catalog fallback ──────► project a pinned events snapshot when missing or stale
```

`CURRENT` pins exact Lance versions for all four tables and records the source URI and source
identity, `fact_version`, `fact_rows`, the source layout revision at build time, projector and
recipe identity, recipe hash, and completeness. `fact_version` and `fact_rows` are the
freshness watermark. Compaction changes only the layout revision and does not stale a projection.
Direct document writes clear lineage; maintenance preserves it.

Incremental sync treats `[previous_fact_rows, fact_rows)` as an append range only because the
canonical manifest validates `fact_rows == total_rows()`. Layout maintenance must preserve both
replacement row count and segment order, so compaction cannot move that logical watermark. After
reading the range, the projector also requires the returned record count to equal the exact range
length; violating any of these proof obligations fails closed instead of silently skipping facts.

Operational commands:

```bash
pchronicle serve --control 127.0.0.1:0 ./trajectory-data
pchronicle status ./trajectory-data --format json
```

Before readiness, `serve` discovers every validated non-empty canonical Store and converges its
deterministic sibling `storyline`. At runtime it discovers new Stores, performs append-compatible
sync or full rebuild as required, and retries bounded failures without blocking durable canonical
writes. A destination without matching lineage is foreign and is never overwritten. `status`
reports `fresh`, `stale`, `missing`, or `error` plus the source watermark and selected generation.

Catalog merges a lineage-linked sidecar with the events source into one logical source. When
`sources.projection_status` is `fresh`, normalized queries use the three tables. When it is `stale`,
Catalog hides the sidecar and falls back to a deterministic projection of the pinned events
snapshot. `projection_generation` exposes the generation actually selected. A Storyline document
store without lineage is never inferred to be a projection of canonical events.

The Gateway-backed Warehouse has an explicit live-read path for point trace observation: after the
Catalog resolves an already discovered canonical source, `/api/events`, `/api/storyline`, and
`/api/trajectory-view` reopen its latest visible events manifest. This does not change the immutable
snapshot semantics of broad SQL queries, and it does not make the derived Storyline sidecar
authoritative.

The projection supervisor is part of `serve`, applies bounded concurrency and retry, and shuts down
with the process. It remains outside the Gateway capture write path, so projection or Catalog
refresh failures cannot block canonical event writes.

本文只负责三表物理 schema、内容层、Snapshot 发布、查询接入和维护语义。事实源与
projection ownership 见[运行存储](trajectory-storage.md)，用户查询流程见
[Dataset 查询指南](../guides/discover-and-query.md)。

这是 pChronicle 唯一的规范化三表模型。旧的 ATIF
`sessions` / `steps` / `tool_calls`、`NormalizedStore` 和内存联表视图已经删除。ATIF
仍作为输入输出格式存在，但查询时先转换为 Storyline，再投影到本页定义的
`runs` / `steps` / `tool_calls` schema，不再维护第二套表结构。

## 表模型

| 表 | 粒度 | 逻辑主键 | 外键 |
|---|---|---|---|
| `runs.lance` | 每个 Storyline 一行 | `document_id` | — |
| `steps.lance` | 每个 turn 一行 | (`document_id`, `step_id`) | `document_id` → runs |
| `tool_calls.lance` | 每个 tool call 一行 | (`document_id`, `step_id`, `call_index`) | (`document_id`, `step_id`) → steps |

`run_id` 是 Run 分组键；一个 Run 可以包含主 Story 和多个 subagent Story，因此
`runs.lance` 中可能有多行共享同一个 `run_id`。内部 `document_id` 使用显式
`trajectory_id`，缺省时回落到 `session_id`，是三表 mutation 的文档作用域键。

常用 JSON 值（message、arguments、metrics、extra 等）以 UTF-8 JSON 列保存；身份、
顺序、类型、时间和性能字段使用独立的 Arrow 标量列，便于过滤和分析。

`runs.schema_version` 与 `runs.origin_json` 保存严格 Storyline wire 版本和来源身份。
`steps.turn_ordinal` 是 turn 数组顺序的权威列；`step_id` 只作身份，不参与重排。
`had_tool_calls` 让显式空数组与字段缺失保持可区分。

`runs.task_json`、`runs.started_at_json`、`runs.finished_at_json`、`runs.prompt_json`、
`runs.extra_json`、`runs.meta_json` 保存文档级 `/task`、文档时间、`/prompt`、`/extra` 与
`/meta`；`steps.env_json`、`steps.finished_at_json`、
`steps.prompt_json` 保存 turn env、结束时间与 turn `/prompt`；
`tool_calls.kind`、`tool_calls.response_json` 保存工具事件类型与 `response`。旧表缺列
时按字段缺失解码。这些对象不拆成独立 SQL 列。

`steps.timestamp` 是规范化到 UTC 的 `Timestamp(Nanosecond, "UTC")` 查询列；
`timestamp_source_json` 保存权威 JSON 标量，因此 RFC3339 字符串和 Unix epoch 秒数值
都能无损恢复。写入端拒绝无效、越界或无法精确表示为纳秒的非空时间。SQL 排序、范围
过滤和时间聚合使用 `timestamp`，重建 Storyline 时使用权威源标量。读取端继续兼容旧的
`Timestamp(Millisecond, "UTC")` 与 `timestamp_rfc3339` 布局。

`steps.observation_json` 保存完整、权威的任意 JSON observation，`had_observation` 保存
出现语义。`tool_calls.results_json` 只是从 `observation.results[]` 可关联项派生的查询列，
不会反向重建 observation；读取时若派生列与权威 observation 不一致会 fail closed。
turn ordinal、call index 也必须从零连续且唯一。

## 大块内容层

### 设计目标与边界

Agent 轨迹中的长 reasoning、工具输出、源代码、日志和多模态载荷会让列式表出现少量超大
cell。若把它们与身份、顺序、类型和指标一起内联，常规过滤与聚合也要承受更大的 fragment、
page cache 和解码开销。pChronicle 因此在三表之外增加共享内容层，但保持三个约束：

1. `runs` / `steps` / `tool_calls` 的 Arrow schema 和 SQL 结果不变；内容层是内部物理优化。
2. 小值继续内联，只有达到阈值的 UTF-8/JSON cell 才外置，避免所有读取都退化成 KV lookup。
3. 内容按原始字节寻址并跨 Storyline 复用；不把轨迹主键、生命周期或业务去重混入内容层。

当前实现没有定制 Lance 文件格式或私有索引类型，而是组合 Lance Blob v2、普通 BTree
scalar index 和 DataFusion execution node。这样可以得到需要的延迟物化能力，同时把维护
面限制在 pChronicle 自己的协议与执行计划中。

### 内部描述符协议

超过默认 64 KiB 的内容列在三表中暂时编码为：

```text
<RS>PCHRONICLE-CONTENT:<type>:<codec>:<blake3-256>:<raw_length>:<preview-base64url>
```

| 字段 | 当前编码 | 作用 |
|---|---|---|
| magic | `PCHRONICLE-CONTENT` | 严格识别内部引用 |
| logical type | `u` / `j` / `b` | UTF-8、JSON；binary 标签已保留给后续二进制列 |
| codec | `i` / `z` | identity 或 Zstd |
| content id | 64 位十六进制 BLAKE3-256 | 对未压缩原始字节寻址、校验和跨轨迹复用 |
| raw length | `u64` | 解压后长度校验，也允许无 payload 的代价判断 |
| preview | URL-safe Base64 | 默认最多 256 个 UTF-8 字节的安全前缀 |

描述符只允许存在于内部物理列。用户原文若恰好以 magic 开头，也会被强制外置，读取时再
恢复为原文，从而消除“用户字符串被误认为引用”的歧义。公开的读取、SQL、转换和导出 API
必须返回完整值或显式 preview，不能泄露描述符。

`objects.lance` 使用以下物理列：

| 列 | 作用 |
|---|---|
| `content_id` | BLAKE3 内容地址；建立 BTree index |
| `logical_type`, `media_type` | 逻辑类型和 MIME 提示 |
| `raw_length`, `stored_length`, `codec` | 完整性检查和存储代价 |
| `preview` | 无 Blob I/O 的安全预览 |
| `payload` | Lance Blob v2，保存 identity/Zstd 字节 |
| `created_at_ms` | 对象创建时间 |

### 写入、复用与发布

写入端对候选 cell 依次执行：

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

对象必须先于引用持久化；`CURRENT` 同时固定三张业务表和对象表的精确 Lance version。
任一步骤失败都不会发布新快照：允许留下不可达对象，但不会发布悬空引用或跨表半提交。
跨轨迹复用只依赖内容地址，不依赖 session 生命周期，因此同一长文本在不同 Run 中只保存
一次。同一写入批次内若 content id 相同但 codec、原始长度或存储字节不一致，会拒绝写入。

对象层在普通写入期间保持 append-only，GC 不进入写入热路径。显式 `maintain` 会只扫描三表
的内容引用列，计算当前快照的可达 content id，并清理不可达 payload。生产环境仍需要把对象
增长率、不可达字节和维护耗时纳入指标。

### 查询期延迟物化

`StorylineDataSource` 先让 Lance 完成业务表的 projection、可安全谓词、scalar index、limit
和并行扫描，再在计划中插入 `ContentHydrationExec`：

- 查询不引用内容列时，不打开 `objects.lance` payload；
- 只收集投影中实际出现的 content id，以最多 512 个为一组走 BTree lookup；
- 根据 row address 批量读取 Blob，解压后验证长度与 BLAKE3，再恢复原 Utf8 列；
- 内容列谓词不能作用于描述符，必须保留在 hydration 之后由 DataFusion 计算；
- `Preview` 模式只返回描述符中的 UTF-8 前缀，零 payload I/O，并拒绝内容列谓词，避免把
  preview 错当成完整值。

因此大内容的成本只由真正读取这些内容的查询承担；身份过滤、计数、分组和指标分析仍沿用
紧凑的三表列式路径。

## 提交布局

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

首次导入创建三张规范化 Lance dataset、共享的 `objects.lance` 和标量索引。后续
`replace_storyline` 不再读取或重写全库，而是按各表主键执行 merge-upsert，并只删除指定
`document_id` 中已经不再存在的旧键。每次替换
会产生一个新的逻辑 snapshot；
`CURRENT` 是一段 JSON，记录必需的 store `schema_version: 2`、逻辑 snapshot id、物理
`table_generation`、三张表以及对象表各自精确的 Lance version id。对象先持久化，三张业务表随后写入，最后才更新 `CURRENT`；
因此失败最多留下不可达对象，不会发布悬空引用或跨表半提交。

阈值、preview 长度和 Zstd level 可通过 `StorylineContentOptions` 配置；三表 schema 不变。

Lance MVCC 的旧版本默认保留，便于已打开的 reader 固定快照及故障恢复。频繁增量更新
会积累 fragment、delete file 和未合并的索引增量。普通 replace 不执行 index refresh 或
compaction，避免某次写请求出现维护型长尾；生产环境通过 `maintain` 显式执行三表并行
compaction、补齐/刷新索引、内容 GC 和按保留期 vacuum。维护产生的四个 dataset version
仍先原子更新 `CURRENT`，之后才回收旧版本和过期的非当前 physical generation。
`CURRENT` 必须是包含 schema version 和全部精确版本的 JSON 指针；缺失或未知 schema
version 会在打开任何 Lance table 前 fail closed，也不读取旧的纯文本 generation 指针。

本地写入通过进程内锁和文件锁串行化；对象存储通过 `CURRENT` 的 ETag/version 条件更新
执行 optimistic CAS。stale commit 不能移动 `CURRENT`；`StorylineLanceStore` 在 CAS 冲突后
直接返回错误，不会重新读取、merge 或自动重试。调用方若选择重试，必须从最新 snapshot 重新
开始完整 replace。上层 lease 可减少冲突，但不改变这一失败语义。

## Rust API

```rust
let store = StorylineLanceStore::open(path).await?;
store.replace_storyline(&storyline).await?;
let restored = store.get_storyline_full("session-id").await?;
let report = store.maintain(&LanceMaintenanceOptions::default()).await?;
```

`replace_storyline` 以 `document_id` 为边界替换三张表中的相关行，同时保留同一 store
内的其他 Storyline。

`get_storyline_full` 明确表示会读取三表并恢复该 Storyline 的全部内容。未被 CLI 或 Web 使用的
store-local 分页 API 已删除；产品层的列表、分页和投影统一由 Catalog、Warehouse API 和
DataFusion query 承担，避免维护第二套不可达的读取协议。

首次导入和替换都并行写三张表。Arrow 行按最多 8192 行一批懒编码并流入 Lance，避免
导入大型语料时同时保留整表的 Arrow 副本。`CURRENT` 只解析一次；DataSource 随后把每张
表直接打开到指针指定的 version，不再先验证、再重复打开同一 dataset。

生产环境通过 `StorylineLanceStore::maintain` Rust API 执行维护；公共 CLI 不提供维护命令。

## DataFusion datasource

`StorylineDataSource` 在打开时固定 `CURRENT` 中的三个业务表 version 和对象表 version，并把三张
dataset 注册为 `runs`、`steps`、`tool_calls`。即使写入端随后切换 `CURRENT`，已经打开
的查询仍使用同一份三表快照。

```rust
let source = StorylineDataSource::open(path).await?;
let ctx = source.session_context()?;
let rows = ctx
    .sql("SELECT step_id, source FROM steps WHERE session_id = 's-1' ORDER BY step_id")
    .await?
    .collect()
    .await?;
```

Datasource 使用 Lance 原生 DataFusion execution plan，支持列裁剪、谓词和 limit 下推，
并采用 unordered physical scan 允许并行读取；有顺序要求的查询必须显式使用
`ORDER BY step_id, call_index`。未引用大内容列的查询不会打开 Blob；引用内容列时在
Lance 投影/安全谓词/limit 之后批量恢复。针对内容列的谓词不下推到内部引用，而是在恢复
后由 DataFusion 求值，确保 SQL 语义不变。内部引用不会由 pChronicle 的读取、查询、导出
API 返回；直接绕过 pChronicle 扫描底层 Lance 文件属于诊断接口，不在该保证内。

预览 UI 可把 `StorylineDataSourceOptions::content_read_mode` 设为
`StorylineContentReadMode::Preview`。该模式直接从描述符返回 UTF-8 安全的短 preview，零
Blob payload I/O；为避免把 preview 当成完整值产生错误结果，内容列谓词在 preview 模式
下会被明确拒绝。

首次创建 table generation 时建立以下标量索引：

| 表 | BTree | Bitmap |
|---|---|---|
| runs | `document_id`, `session_id`, `run_id` | — |
| steps | `document_id`, `session_id`, `timestamp` | `effective_kind`, `source` |
| tool_calls | `document_id`, `session_id`, `tool_call_id` | `function_name` |

这些索引针对按 Story/Run 定位、tool-call 查找和类型过滤。`step_id` 在每个 Storyline 内
从小值重新计数，全局选择性低，因此不单独建立 BTree；组合条件先用 `session_id` 定位到
单个 Storyline，再过滤很短的 step 范围。DataFusion 的索引谓词会下推为 Lance
`ScalarIndexQuery`。

`StorylineDataSourceOptions` 可显式控制 `use_scalar_indexes` 与 `scan_in_order`；默认配置
面向在线分析查询启用索引、关闭物理顺序。关闭索引主要用于 benchmark、诊断或极小表
的全扫描对照。

## 统一查询引擎

`ChronicleQueryEngine` 是对外的只读 SQL 门面。All six disk formats (Canonical
Event, Storyline Lance, AgenticMD, ATIF, OpenAI Msg, ACTF) open through the
single entry `ChronicleQueryEngine::open(format, path, options)` and register
the semantically matching query tables, so SQL does not change with the
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
`runs`/`steps`/`tool_calls` are not registered live by default — Storyline
query surfaces prefer the lineage-fresh Storyline Lance projection, and
without one a bounded row/byte-budget fallback runs (budget exhaustion is an
explicit error, never a silent truncation). The other five formats register
`runs`/`steps`/`tool_calls`.

`query` 返回 Arrow `RecordBatch`，适合服务端继续处理；`dataframe` 返回 lazy DataFrame，
适合追加 DataFusion 变换或查看计划；`query_jsonl` 用于 CLI/API 边界。调用者也可通过
`context()` 取得 `SessionContext` 注册 UDF 或额外表。`backend_info()` returns a
`QueryBackendInfo` that reports `format` / `tables` / `capabilities` /
`snapshot` per the provider's real implementation; filter pushdown capability
distinguishes `Unsupported` / `Inexact` / `Exact` / `ExpressionDependent` and
is never overstated.

The unified document source entry `open_document(format, path)` accepts a
single ATIF JSON object, a JSON array, JSONL/NDJSON with one complete
trajectory per line, and directories containing such ATIF documents. File
paths register as per-file lazy `StreamingTable`s by default: the manifest
freezes paths and file identities at open time and scans read only the hit
files; directories are discovered in stable order, each file is an
independent partition, and fixed-size Arrow batches provide backpressure.

### pChronicle + JSON 投影查询快路径

旧路径把每份 JSON 完整解析为格式对象，再执行 `ATIF → Storyline → 三表行 → 全宽 Arrow`
后交给 SQL。即使查询只需要 `source` 或 `COUNT(*)`，也会构造 message、reasoning、metrics、
tool calls 等未使用的大字段。新路径把优化边界前移到 `TableProvider::scan`：

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

当前 fast path 的适用范围是 ATIF 单对象、数组（包括 pretty JSON）和 JSONL/NDJSON，
以及 ACTF 单对象和数组；目标表为 `steps`，并且物理计划存在严格列裁剪。它有意保持保守：

| 输入/查询 | 执行路径 |
|---|---|
| ATIF object/pretty object + projected `steps` | reader-backed seeded projected decoder |
| ATIF array/pretty array + projected `steps` | `fill_buf` 结构扫描 + 有界 element buffer + seeded `from_slice` |
| ATIF JSONL/NDJSON + projected `steps` | `BufRead` 逐记录、有界 record buffer |
| ACTF object/array + projected `steps` | reader/slice seeded projected decoder |
| `_file_`、`session_id`、`step_id`、`source` 的安全简单谓词 | 可提前裁剪，DataFusion 仍复核 |
| `SELECT *` | 完整规范化 fallback |
| `runs` / `tool_calls` | 完整规范化 fallback |
| OpenAI-message | 完整规范化 fallback |
| 无法证明安全的表达式、OR/函数/跨列条件 | 不预裁剪，由 DataFusion 求值 |

`DeserializeSeed` 把查询 projection 和安全谓词传入 `Visitor`；未引用字段交给
`IgnoredAny` 做语法扫描，不构造 `Value`/Storyline。ATIF JSONL/NDJSON 以 `BufRead`
逐记录读取；JSON array 的结构扫描器识别字符串和转义，在不构造 DOM 的情况下提取单个
trajectory/document，再通过 slice decoder 执行投影解析。调用方可显式设置
`max_record_bytes` 限制单个 document/record；默认不设单记录上限，只保留
`max_file_bytes` 文件边界。三种路径都不先复制整文件。
Arrow encoder 也只创建投影列，`COUNT(*)` 使用合法的零列 batch。轻量路径
校验 JSON、必需字段、重复 session、命中文档内的重复 step 和当前表内约束；跨表引用
完整性仍由导入路径或完整 fallback 负责。这一边界使临时查询不承担导入语义，同时不降低
SQL 结果正确性。

查询指标额外报告 `projected_files`、`streamed_records`、`streaming_buffer_peak_bytes`、
scanned/pruned documents、scanned/pruned/emitted rows 和 `projected_arrow_bytes`，用来区分
“源字节扫描”“输入缓冲”“JSON 字段物化”和“Arrow 输出”四个成本。仓库 benchmark 报告
median/P95、rows/s、独立进程峰值 RSS，以及计数 allocator 观测到的 allocation calls/bytes；
这些是指定 corpus、查询和机器的回归数据，不是跨环境 SLA。

该路径仍需顺序扫描命中文件的全部 JSON 字节，不是文件内索引。一次性或受控批次查询可
直接使用 JSON；超大、远端或反复查询的数据应先转换为 Lance，利用 snapshot、列裁剪、
并行 fragment scan 和 scalar index。

ATIF 导入同样默认走 `AtifReader`。空 store 使用一个 producer 单遍完成
校验、Storyline 规范化和三表拆分，再经三条有界 Arrow channel 并行创建三个 Lance
dataset；已有 store 则以最多 256 个 Storyline 为一个增量替换批次。两种路径都在所有
输入和三表写入成功后才原子切换一次 `CURRENT`。

CLI 使用相同引擎，输出稳定的 JSONL：

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

查询是只读的；SQL 可以使用 SELECT、CTE、JOIN、聚合和 DataFusion 内置函数，但不通过
这个门面执行 DDL/DML。Lance 引擎打开时固定 `CURRENT` 指向的三个版本，从而保证一次
查询会话内三张表来自同一快照。

仓库使用 Criterion.rs + hyperfine 的统一 benchmark runner。Criterion 负责 CPU-bound
转换、events→Storyline 和三表 split/reconstruct 微基准；canonical event append、投影
build/sync/verify、Lance/DataFusion 生命周期、JSON streaming 与 RSS 场景由 hyperfine
重复执行独立进程，最终生成统一 JSON、Markdown 和 HTML：

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

`raw-report.json` 以 `$["measurements"]...` JSONPath 地址保存原始指标和环境，
`bencher.json` 是历史平台使用的扁平投影，
`report.md` 写入 GitHub Actions Job Summary，`report.html` 与 Criterion 明细作为 artifact。

JSON 对照使用单个 NDJSON 文件，避免大量小文件打开开销。ATIF `steps` 直接查询会把
DataFusion projection 和可安全预裁剪的 `session_id`、`step_id`、`source` 谓词传给
projected decoder：未引用 JSON 字段只做语法扫描，不构造 Storyline/三表对象，Arrow
batch 也只包含执行计划需要的列。object、array、pretty JSON 和 JSONL/NDJSON 共用流式
projection decoder；ACTF `steps` 也使用对应的 projected decoder；`SELECT *`、其他表和
OpenAI-message 仍走完整规范化 fallback。轻量路径执行 JSON、必需字段和表内约束校验，跨表引用完整性由导入或完整
fallback 校验。预解析内存 JSON 对照只计算查询逻辑，用来区分产品工作流与纯内存遍历成本。
benchmark 还单独输出 DataSource 冷打开并执行 SQL、`get_storyline_full` 点查和单 Storyline
替换的延迟，避免 warm SQL 吞吐掩盖在线读写路径的写放大。

性能结论不应写成“Lance 在所有规模和查询上必然更快”：显式构造的 `MemTable` 或预解析
内存 JSON 在小数据下仍可能更快。默认 ATIF streaming 解决的是内存上界，不提供物理
索引；Lance 的主要优势仍是更小的物理体积、近乎常数的 datasource 打开时间，以及列
裁剪、并行扫描和选择性索引收益。

## 相关文档

- [事实、Projection 与 Revision](../concepts/facts-and-projections.md)：解释 Storyline 为何是 projection。
- [pChronicle 架构](architecture.md)：定义 publication 和 read consistency 保证。
- [Dataset Catalog](catalog.md)：说明 Source discovery 与固定 Snapshot 如何打开本 Store。
- [`pchronicle` 参考](../reference/cli.md)：当前对外查询、导入导出与服务命令。
