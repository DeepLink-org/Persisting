# Storyline 三表 Lance 存储

`StorylineLanceStore` 是 pChronicle 的 Storyline-native 规范化物理表示。它与
`events.lance` 原始事件日志并列存在，不替代后者。

这是 pChronicle 唯一的规范化三表模型。旧的 ATIF
`sessions` / `steps` / `tool_calls`、`NormalizedStore` 和内存联表视图已经删除。ATIF
仍作为输入输出格式存在，但查询时先转换为 Storyline，再投影到本页定义的
`runs` / `steps` / `tool_calls` schema，不再维护第二套表结构。

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
    └── <table-generation>/
        ├── runs.lance/
        ├── steps.lance/
        └── tool_calls.lance/
```

首次导入创建三张 Lance dataset 和标量索引。后续 `replace_storyline` 不再读取或重写
全库，而是按 `session_id` 删除旧行、追加新行。每次替换会产生一个新的逻辑 snapshot；
`CURRENT` 是一段 JSON，记录逻辑 snapshot id、物理 `table_generation`，以及三张表各自
精确的 Lance version id。三张表的新版本全部持久化后才更新 `CURRENT`，所以读者只会
看到完整的旧版本元组或新版本元组，不会看到跨表半提交。

Lance MVCC 的旧版本默认保留，便于已打开的 reader 固定快照及故障恢复。频繁增量更新
会积累 fragment、delete file 和未合并的索引增量。在线替换达到 32 个 fragment 时会做
一次自动 index refresh 与 compaction；长期运行再通过 `maintain` 显式执行三表并行
compaction、补齐/刷新索引和按保留期 vacuum。维护产生的三个新 version 仍先原子更新
`CURRENT`，之后才回收旧版本。旧版仅包含 generation 名称的纯文本 `CURRENT` 仍可读取，
下一次成功写入会升级为版本元组。

当前写入在进程内串行化。跨进程 writer 仍需由上层提供单 writer 或租约约束。

## Rust API

```rust
let store = StorylineLanceStore::open(path).await?;
store.replace_storyline(&storyline).await?;
let restored = store.get_storyline("session-id").await?;
let report = store.maintain(&LanceMaintenanceOptions::default()).await?;
```

`replace_storyline` 以 `session_id` 为边界替换三张表中的相关行，同时保留同一 store
内的其他 Storyline。

首次导入和替换都并行写三张表。Arrow 行按最多 8192 行一批懒编码并流入 Lance，避免
导入大型语料时同时保留整表的 Arrow 副本。`CURRENT` 只解析一次；DataSource 随后把每张
表直接打开到指针指定的 version，不再先验证、再重复打开同一 dataset。

生产环境也可使用 pPilot 执行相同维护：

```bash
ppilot chronicle maintain ./storyline-store \
  --vacuum-retention-hours 168 \
  --target-rows-per-fragment 1048576
```

## DataFusion datasource

`StorylineDataSource` 在打开时固定 `CURRENT` 中的三个精确 Lance version，并把三张
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
`ORDER BY step_id, call_index`。

首次创建 table generation 时建立以下标量索引：

| 表 | BTree | Bitmap |
|---|---|---|
| runs | `session_id`, `run_id` | — |
| steps | `session_id` | `effective_kind`, `source` |
| tool_calls | `session_id`, `tool_call_id` | `function_name` |

这些索引针对按 Story/Run 定位、tool-call 查找和类型过滤。`step_id` 在每个 Storyline 内
从小值重新计数，全局选择性低，因此不单独建立 BTree；组合条件先用 `session_id` 定位到
单个 Storyline，再过滤很短的 step 范围。DataFusion 的索引谓词会下推为 Lance
`ScalarIndexQuery`。

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
JSONL/NDJSON，以及包含这些 ATIF 文档的目录。文件路径默认注册为 DataFusion
`StreamingTable`：打开时以有界内存遍历一次完成全量校验和计数；每次 scan 重新打开
输入，NDJSON 逐行解析，目录按稳定顺序逐文件消费，并以固定大小 Arrow batch 提供背压。
它不会把完整 ATIF corpus 常驻内存；为维持 `session_id` 唯一性约束，只保留已见过的
session key 集合。显式 `from_json` / `from_trajectories` 因为调用者已经持有完整输入，
仍使用 `MemTable`。`.json` 对象和数组用于格式兼容，会按单个文件缓冲；超大数据集应写成
NDJSON，才能保持内容内存有界。

`ppilot chronicle import` 同样默认走 `AtifReader`。空 store 使用一个 producer 单遍完成
校验、Storyline 规范化和三表拆分，再经三条有界 Arrow channel 并行创建三个 Lance
dataset；已有 store 则以最多 256 个 Storyline 为一个增量替换批次。两种路径都在所有
输入和三表写入成功后才原子切换一次 `CURRENT`。

CLI 使用相同引擎，输出稳定的 JSONL：

```bash
ppilot query sql ./trajectories.ndjson \
  --sql 'SELECT source, COUNT(*) AS steps FROM steps GROUP BY source ORDER BY source'

# 含 CURRENT 的三表 store 根目录会被 auto 识别为 Lance
ppilot query sql ./storyline-store \
  --sql 'SELECT step_id, source FROM steps WHERE session_id = '\''s-1'\'' ORDER BY step_id'
```

查询是只读的；SQL 可以使用 SELECT、CTE、JOIN、聚合和 DataFusion 内置函数，但不通过
这个门面执行 DDL/DML。Lance 引擎打开时固定 `CURRENT` 指向的三个版本，从而保证一次
查询会话内三张表来自同一快照。

仓库提供两组可执行 benchmark：

```bash
# scalar index 与 full scan A/B
cargo bench -p persisting-pchronicle --bench atif_storyline_lance

# 导入、冷查询、点查、增量替换，以及 warm SQL 对比
PCHRONICLE_BENCH_SCALE=128 PCHRONICLE_BENCH_ITERS=30 \
  cargo bench -p persisting-pchronicle --bench lance_vs_json
```

JSON 对照使用单个 NDJSON 文件，避免大量小文件打开开销。ATIF/DataFusion streaming
对照在每次 scan 中包含文件读取、Serde 解析、Storyline 规范化和查询计算；预解析内存
JSON 对照只计算查询逻辑，用来区分存储引擎收益与纯内存遍历成本。Datasource open 还会
单独执行一次有界校验/计数扫描，因此冷打开与实际 SQL scan 是两个明确阶段。
benchmark 还单独输出 DataSource 冷打开并执行 SQL、`get_storyline` 点查和单 Storyline
替换的延迟，避免 warm SQL 吞吐掩盖在线读写路径的写放大。

性能结论不应写成“Lance 在所有规模和查询上必然更快”：显式构造的 `MemTable` 或预解析
内存 JSON 在小数据下仍可能更快。默认 ATIF streaming 解决的是内存上界，不提供物理
索引；Lance 的主要优势仍是更小的物理体积、近乎常数的 datasource 打开时间，以及列
裁剪、并行扫描和选择性索引收益。
