# Storyline 三表 Lance 存储

`LanceStorylineStore` 是 pChronicle 的 Storyline-native 规范化物理表示。它与
`events.lance` 原始事件日志并列存在，不替代后者。

## 表模型

| 表 | 粒度 | 逻辑主键 | 外键 |
|---|---|---|---|
| `runs.lance` | 每个 Storyline 一行 | `session_id` | — |
| `steps.lance` | 每个 turn 一行 | (`session_id`, `step_id`) | `session_id` → runs |
| `tool_calls.lance` | 每个 tool call 一行 | (`session_id`, `tool_call_id`) | (`session_id`, `step_id`) → steps |

`run_id` 是 Run 分组键；一个 Run 可以包含主 Story 和多个 subagent Story，因此
`runs.lance` 中可能有多行共享同一个 `run_id`。`session_id` 才是 Storyline 文档的
唯一键。

常用 JSON 值（message、arguments、metrics、extra 等）以 UTF-8 JSON 列保存；身份、
顺序、类型、时间和性能字段使用独立的 Arrow 标量列，便于过滤和分析。

tool result 不再留在 step 的 observation JSON 中。写入时根据
`observation.results[].source_call_id` 关联到对应 tool call，并保存到该行的
`results_json`。缺失或错误的关联会拒绝整次写入。

## 提交布局

```text
root/
├── CURRENT
└── generations/
    └── gen-.../
        ├── runs.lance/
        ├── steps.lance/
        └── tool_calls.lance/
```

三张表先写入新的不可变 generation，全部成功后再原子替换 `CURRENT`。因此读者只会
看到旧 generation 或新 generation，不会看到跨表半提交。旧 generation 默认保留，
便于回滚和故障分析；后续可增加显式 vacuum 策略。

当前写入在进程内串行化。跨进程 writer 仍需由上层提供单 writer 或租约约束。

## Rust API

```rust
let store = LanceStorylineStore::open(path).await?;
store.replace_storyline(&storyline).await?;
let restored = store.get_storyline("session-id").await?;
```

`replace_storyline` 以 `session_id` 为边界替换三张表中的相关行，同时保留同一 store
内的其他 Storyline。

## DataFusion datasource

`StorylineDataSource` 在打开时固定一个已提交 generation，并把三张 Lance dataset 注册
为 `runs`、`steps`、`tool_calls`。即使写入端随后切换 `CURRENT`，已经打开的查询仍使用
同一份三表快照。

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
`ORDER BY step_id, call_index`。

每个 generation 写入时创建以下标量索引：

| 表 | BTree | Bitmap |
|---|---|---|
| runs | `session_id`, `run_id` | — |
| steps | `session_id`, `step_id` | `effective_kind`, `source` |
| tool_calls | `session_id`, `step_id`, `tool_call_id` | `function_name` |

这些索引针对按 Story/Run 定位、step 范围扫描、tool-call 查找和类型聚合。DataFusion
的组合谓词会下推为 Lance `ScalarIndexQuery`。

`StorylineDataSourceOptions` 可显式控制 `use_scalar_indexes` 与 `scan_in_order`；默认配置
面向在线分析查询启用索引、关闭物理顺序。关闭索引主要用于 benchmark、诊断或极小表
的全扫描对照。

## 统一查询引擎

`ChronicleQueryEngine` 是对外的只读 SQL 门面。Lance 与 ATIF 后端注册完全相同的
`runs`、`steps`、`tool_calls` 表，因此查询语句不需要随物理格式改变：

```rust
let lance = ChronicleQueryEngine::open_lance("./storyline-store").await?;
let batches = lance.query(
    "SELECT session_id, step_id, source FROM steps WHERE step_id >= 10"
).await?;

let atif = ChronicleQueryEngine::open_atif("./trajectories.ndjson")?;
let jsonl = atif.query_jsonl(
    "SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source"
).await?;
```

`query` 返回 Arrow `RecordBatch`，适合服务端继续处理；`dataframe` 返回 lazy DataFrame，
适合追加 DataFusion 变换或查看计划；`query_jsonl` 用于 CLI/API 边界。调用者也可通过
`context()` 取得 `SessionContext` 注册 UDF 或额外表。

`AtifDataSource` 接受单个 ATIF JSON 对象、JSON 数组、每行一个完整 trajectory 的
JSONL/NDJSON，以及包含这些 ATIF 文档的目录。输入在打开时
完成校验、Storyline 规范化和 Arrow 分批构建，之后的 SQL 在内存 `MemTable` 上执行。

CLI 使用相同引擎，输出稳定的 JSONL：

```bash
ppilot query ./trajectories.ndjson --source atif \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'

# 含 CURRENT 的三表 store 根目录会被 auto 识别为 Lance
ppilot query ./storyline-store \
  --sql 'SELECT step_id, source FROM steps WHERE session_id = '\''s-1'\'' ORDER BY step_id'
```

查询是只读的；SQL 可以使用 SELECT、CTE、JOIN、聚合和 DataFusion 内置函数，但不通过
这个门面执行 DDL/DML。Lance 引擎打开时固定 `CURRENT` 指向的 generation，从而保证
一次查询会话内三张表来自同一快照。

仓库提供两组可执行 benchmark：

```bash
# scalar index 与 full scan A/B
cargo bench -p persisting-pchronicle --bench atif_storyline_lance

# Lance/DataFusion 与落盘 JSON、预解析内存 JSON 对比
PCHRONICLE_BENCH_SCALE=128 PCHRONICLE_BENCH_ITERS=30 \
  cargo bench -p persisting-pchronicle --bench lance_vs_json
```

JSON 对照使用单个 NDJSON 文件，避免大量小文件打开开销。落盘对照包含文件读取、Serde
解析和查询计算；预解析内存对照只计算查询逻辑，用来区分存储引擎收益与纯内存遍历成本。
ATIF/DataFusion 对照把解析成本计入 datasource open、但不重复计入每次 warm query，
因此适合衡量常驻服务查询；JSON read+Serde 才是“每次从物理 JSON 查询”的直接对照。

性能结论不应写成“Lance 在所有规模和查询上必然更快”：小数据全部装入内存后，
`MemTable` 全扫描没有磁盘和索引调度成本，可能胜过 Lance。Lance 的主要优势是显著更小
的物理体积、近乎常数的 datasource 打开时间、无需把完整 ATIF 常驻内存，以及随着数据
规模增长而显现的列裁剪、并行扫描和选择性索引收益。
