# pChronicle Dataset Catalog 设计

> 当前实现说明。Dataset 命令参数见 [`pchronicle` 命令参考](../reference/cli.md)；轨迹物理格式见
> [pChronicle 轨迹存储](trajectory-storage.md) 与
> [Storyline 三表 Lance](storyline-lance.md)。

## 1. 定位

pChronicle Dataset Catalog 面向由多个存储位置和多种轨迹格式共同组成的查询空间，主要
覆盖以下场景：

- 同时查询在线数据、历史归档和评测数据；
- 一个逻辑数据集下包含多层目录、多个 Run 级 `events.lance`，以及若干外围 JSON 文件；
- 不同数据集存在相同的 `run_id`、`session_id` 或文件名；
- Web 服务需要在多次请求之间复用同一份发现结果，并且只在显式刷新后切换视图。

它位于存储 URI 与 DataFusion SQL 之间，提供轻量的命名和发现边界。用户把若干 URI
挂载为 Dataset；pChronicle 对每个挂载递归发现轨迹源，将不同物理格式统一投影到稳定表，
并在一次查询或一代 Web 快照内固定其成员与版本。

Catalog **不是**一份需要长期维护的元数据数据库。它不复制源数据、不接管对象存储目录、
不声明外围 JSON 已成为 canonical 数据，也不要求后台同步任务。

## 2. 目标与非目标

### 2.1 目标

1. **多 Dataset 联查**：一次 SQL 可以访问多个具名本地目录或对象存储前缀。
2. **层级发现**：一个 Dataset 的 URI 可以指向存储根、Run 根、复合 store 或单个文件。
3. **统一表模型**：Storyline、canonical events、ATIF、OpenAI messages 和 ACTF 使用相同
   的查询表名。
4. **稳定身份**：任何 Storyline 都能用 `(dataset, _file_, session_id)` 唯一定位到快照内的物理源。
5. **快照一致性**：一条查询不会在执行中混入新发现的文件或新的 Lance generation。
6. **稳定默认入口**：位置参数固定挂载为名为 `dataset` 的默认 Dataset。
7. **有界失败**：发现、格式检测、单文件大小、解析并发和查询内存都有显式限制与错误策略。
8. **安全写入**：命名挂载默认只读；服务端写操作只能落到显式选定的 canonical events
   Dataset。
9. **Catalog-aware 裁剪**：先用 Dataset 和 `_file_` 条件选出 source，再构造物理扫描计划。
10. **惰性解析**：Catalog 快照只固定成员和版本描述；Lance dataset、远程对象和文件
    datasource 在查询确实需要时才打开，并在快照内 single-flight 复用。

### 2.2 非目标

- 不提供 Hive Metastore、Glue Catalog 一类持久化 catalog service。
- 不在 Catalog 中建立跨文件索引、统计信息仓库或 materialized view。
- 不把不同源中的同名 `run_id` 或 `session_id` 自动合并。
- 不为多个独立物理源提供分布式事务或全局时间点读。
- 不通过 Catalog 修改、搬运或转换外围 JSON；需要长期列式分析时仍应显式导入 Lance。
- 不在 URI 参数中管理密钥；对象存储认证继续使用对应 SDK 的标准凭证链。

## 3. 核心模型

![Dataset Catalog 查询路径](../../assets/diagrams/persisting/dataset-catalog.svg)

核心对象分为七层：

| 对象 | 作用 | 生命周期 |
|---|---|---|
| `DatasetMount` | 保存层级 namespace、SQL alias、根 URI 和可选格式提示 | 配置期 |
| `CatalogDataset` | 一个 Dataset 及其 `DiscoveredSource` 列表 | 快照期 |
| `DiscoveredSource` | 描述一个复合 store 或外围文件的逻辑路径、格式、版本和状态 | 快照期 |
| `DatasetCatalogSnapshot` | 固定全部挂载的成员、源版本和临时对象文件 | 一条 CLI 查询或一代 Server Catalog |
| `LazySource` | 保存固定 source 描述，并并发安全地缓存首次解析结果或错误 | 与快照相同 |
| `CatalogTableProvider` | 在 DataFusion `scan` 边界执行 source 裁剪并组合命中的物理计划 | 每个 Dataset 稳定表 |
| `ChronicleQueryEngine` | 把快照注册为 DataFusion schema，并执行只读 SQL | 与快照相同 |

### 3.1 Namespace、Dataset 与 SQL alias

Dataset 是用户命名的逻辑查询空间，不等于物理 Lance dataset。一个 Dataset 可以包含多个
Storyline store、多个 `events.lance` 和多个外围文件；每个 Dataset 对应一个 DataFusion
schema。Catalog 用 `NamespacePath` 表达层级身份，用独立的 SQL alias 注册 DataFusion
schema；因此 `prod/agents` 与 `staging/agents` 可以同时存在，而无需把目录身份编码进表名。

SQL alias 会去除首尾空白并转成小写，必须匹配 `[A-Za-z_][A-Za-z0-9_]*`。`public` 和
`information_schema` 是保留名称。namespace component 允许字母、数字、`_`、`-`、`.`。
完整 namespace 或 SQL alias 重名都会在发现前失败。

### 3.2 Source 与 `_file_`

Source 是 Catalog 的最小发现单元：

- Storyline `CURRENT` 根是一个 `store` source；
- canonical `events.lance` 根是一个 `store` source；
- 每个 JSON、JSONL 或 NDJSON 文件是一个 `file` source。

`_file_` 是 source 相对 Dataset 根的 UTF-8 逻辑路径，统一使用 `/` 分隔。挂载根自身作为
source 时使用 `.`。它不是源表的持久字段，也不会写回 Lance。

### 3.3 Storyline 与 Run 身份

`session_id` 是 Storyline 的逻辑主键，但只在一个 source 内保证唯一；外围文件或不同归档
中可以出现相同值。因此 Catalog 和 Server 使用以下复合键：

```text
(dataset, _file_, session_id)
```

`run_id` 是 Run 分组键，一个物理 Run 可以包含主 Storyline 与多个 subagent Storyline，
所以同一 source 内多行可以共享一个 `run_id`。canonical events 规范化时按事件的
Storyline/session 身份分组，并保留实际 `events.lance` URI，避免后续读写根据挂载根猜测
物理位置。

### 3.4 Source revision

内部用 `CatalogSourceRevision` 保存类型化 revision，而不是让一个字符串同时表示 Storyline
generation、event fact/layout 水位、本地文件指纹与对象版本。`sources.snapshot_ref` 仍是便于
SQL 展示和筛选的字符串投影；一致性判断、快照摘要和 API 描述使用类型化 revision。

一个 canonical events source 可以关联多个派生 Storyline 投影。Catalog 不因此隐藏或拒绝
canonical source，而按 `fresh → last_modified → generation → path` 的稳定顺序选出一个读取
加速投影；`projection_candidates` 暴露候选数。所有候选都不新鲜时，查询回退到固定的
canonical events 快照。

## 4. 挂载与默认 Dataset

### 4.1 CLI 形式

`--dataset` 可以重复：

```bash
pchronicle query \
  --dataset current=local:///srv/pchronicle/current \
  --dataset archive=s3://trajectory-bucket/archive \
  "SELECT * FROM current.runs"
```

也可以从 TOML 读取：

```toml
[datasets]
current = "local:///srv/pchronicle/current"
archive = "s3://trajectory-bucket/archive"
```

```bash
pchronicle query --dataset current=local:///srv/pchronicle/current \
  --dataset archive=s3://trajectory-bucket/archive \
  "SELECT table_schema, table_name FROM information_schema.tables"
```

位置参数、配置文件与重复 `--dataset` 可以同时使用。三者中出现规范化重名时整体失败，
不会按参数顺序覆盖。

### 4.2 默认选择规则

| 输入 | 默认 Dataset | 不带 schema 的 `runs` 等表名 |
|---|---|---|
| 有位置参数 `INPUT` | 固定为 `dataset` | 指向 `dataset.runs` 等默认 view |
| 无位置参数且只有一个命名挂载 | 唯一挂载 | 指向该 Dataset 的默认 view |
| 无位置参数且有多个命名挂载 | 无 | 必须写 `current.runs` 等限定名 |

位置参数形式如下：

```bash
pchronicle query ./capture "SELECT * FROM dataset.runs"
```

等价于把 `./capture` 挂载为 `dataset`，并查询 `dataset.runs`。它还可以追加其他挂载：

```bash
pchronicle query ./capture \
  --dataset archive=s3://trajectory-bucket/archive \
  "SELECT * FROM dataset.runs UNION ALL SELECT * FROM archive.runs"
```

## 5. 层级发现

### 5.1 示例

假设挂载目录如下：

```text
capture-root/
├── live/
│   └── CURRENT
├── agents/
│   └── codex/
│       └── run-001/
│           └── events.lance/
│               └── _manifest.json
└── imports/
    ├── batch-a.atif.jsonl
    └── nested/
        └── session.json
```

Catalog 产生四个 source：

| `_file_` | `kind` | 可能的 `format` |
|---|---|---|
| `live` | `store` | `storyline` |
| `agents/codex/run-001/events.lance` | `store` | `events` |
| `imports/batch-a.atif.jsonl` | `file` | `atif` |
| `imports/nested/session.json` | `file` | 按文件检测 |

`live` 和 `events.lance` 的内部文件不会再次成为 source。这一“识别复合根后停止下探”的规则
避免把 manifest、generation、segment 或 `objects.lance` 错当成用户输入。

### 5.2 本地发现

本地 URI 支持普通路径、`local://` 和 `file://`：

1. 如果根是 `.json`、`.jsonl` 或 `.ndjson` 文件，直接建立单个 source。
2. 如果根目录包含 `CURRENT`，整个根是一个 Storyline source。
3. 如果根名为 `events.lance` 且包含 `_manifest.json`，整个根是一个 events source。
4. 否则按稳定路径顺序递归目录：识别复合根，或收集支持的外围文件。
5. 符号链接不会跟随，避免循环、越界读取和同一物理文件的重复身份。

### 5.3 对象存储发现

对象 URI 通过 Lance/object-store 适配层解析。Catalog 流式消费前缀 listing，并在读取
`max_entries + 1` 个对象前失败；不会先把无界 listing 收进内存再检查。随后：

1. 用 `CURRENT` 对象识别 Storyline 根；
2. 用 `events.lance/_manifest.json` 识别 canonical events 根；
3. 排除所有复合根内部对象；
4. 把剩余 `.json`、`.jsonl` 和 `.ndjson` 对象作为独立 source；
5. 按 Dataset 相对 object key 排序。

当前对象后端沿用 pChronicle/Lance 支持的 URI scheme，例如 `s3://`、`az://` 和 `gs://`。
挂载或 listing 本身失败意味着无法建立可信成员集，即使使用 `report` 模式也会失败。

### 5.4 格式检测

每个外围文件独立检测格式，因此同一个 Dataset 可以混合 ATIF、OpenAI messages 与 ACTF。
位置参数配合显式 `--source` 时，该值作为默认 Dataset 的格式约束：复合 store 类型或文件
检测结果不匹配会报错。命名 Dataset 当前使用自动检测。

本地和远程外围文件都不会为了自动检测而在 Catalog 构建期读取内容；如果没有显式格式
提示，`sources.format` 可以是 `NULL`。Catalog 会先冻结本地文件指纹或远程对象版本，等
`_file_` 裁剪选中该 source 后才做有界格式检测。检测结果与 datasource 解析结果一起缓存
在快照的 `LazySource` 中。

## 6. SQL Provider

Dataset 的稳定公共关系、精确 `sources` column、Source-local identity 与 join 规则属于
[Query Model Reference](../reference/query-model.md)。本节只解释 Catalog 如何为这些关系
构造执行计划。

外围文件不会生成伪造的原始 event 行；它们只能通过 Storyline 规范化关系查询。Catalog
为实体关系增加常量 `_file_`，把公开 Source identity 连接到惰性物理 Source。

### 6.1 Catalog-aware source pruning

`runs`、`steps`、`tool_calls` 和 `events` 各自由一个 Dataset 级
`CatalogTableProvider` 提供。DataFusion 把投影、过滤条件和 limit 交给 provider 后，
provider 按以下顺序构造物理计划：

1. 先排除不能提供目标表的 source，例如 `events` 自动排除 Storyline 和外围文件；
2. 在每个 source 的常量 `_file_` 上求值可识别的过滤表达式；
3. 完全不可能匹配的 source 直接跳过，不调用 `LazySource::resolve`；
4. 以 `max_concurrent_sources` 为上限、按稳定 source 顺序解析候选，并把业务列投影、业务
   谓词和 limit 继续交给其原生 provider；
5. 零个命中 source 生成 `EmptyExec`，一个命中 source 直接使用其计划，多个命中 source
   才生成 `UnionExec`，最后在需要时应用全局 limit。

可精确用于 source 裁剪的 `_file_` 谓词包括 `=`、`!=`、`IN`、`NOT IN`、大小写敏感的
`LIKE`/`NOT LIKE`，以及由 `AND`、`OR`、`NOT` 组成且能安全求值的组合。对同时包含 source
条件和业务条件的表达式采取保守三值判断：只有能证明该 source 不可能匹配时才跳过。
例如：

```sql
SELECT run_id, session_id
FROM archive.runs
WHERE _file_ LIKE '2026/08/%'
  AND session_id = 'session-42';
```

这里 `LIKE` 在 Catalog 层裁剪 source，`session_id` 则下推到命中 source 的 Lance 或文件
provider。没有 `_file_` 条件时，Catalog 没有跨 source 的 `run_id`/时间统计信息，必须把
目标表的全部兼容 source 视为候选；业务谓词仍可在每个原生 provider 内下推。

`LazySource` 使用异步 `OnceCell` 缓存解析结果，多个并发查询命中同一个 source 时只执行
一次打开、远程物化或格式解析。source 解析阶段的失败也会缓存，以保证同一快照内行为
稳定。canonical events 的原始 `events` 表可以直接扫描固定 segment。没有 fresh 投影时，
带可证明 `session_id = ...` 或 `session_id IN (...)` 的查询只读取目标 Storyline 的完整历史；
宽查询读取固定 snapshot。两种 fallback 都受 `max_event_fallback_rows` 和
`max_event_fallback_bytes` 约束，且只物化当前查询请求的关系表；超限时要求 build/sync
Storyline 投影。
`load_events` 点查直接读取目标 session，不构造 DataFusion MemTable，但使用相同的行数和
字节预算。

可以用 `EXPLAIN` 检查裁剪后的物理计划：精确命中一个 source 时，计划中不应出现
`UnionExec`。

### 6.2 联查规则

同一 Dataset 可以包含多个物理 source，而 `run_id`/`session_id` 只保证在单个 source 中
有效。两个内建轨迹表跨多个同 Dataset source 联接时，必须显式加入 `_file_` 等值：

```sql
SELECT r.run_id, s.step_id, s.message_json
FROM archive.runs r
JOIN archive.steps s
  ON r._file_ = s._file_
 AND r.session_id = s.session_id;
```

遗漏 `_file_` 会在执行前被拒绝。跨 Dataset 联接不要求 `_file_` 相同，因为左右命名空间
已经不同，通常也不会拥有相同的目录布局：

```sql
SELECT c.run_id, a.run_id AS archived_run
FROM current.runs c
JOIN archive.runs a ON c.session_id = a.session_id;
```

校验作用于 `runs`、`steps` 和 `tool_calls` 的内建联接；与用户通过 `--table` 注册的维表
联接不受该规则影响。查询引擎只接受单条只读 `SELECT`、`VALUES`、`DESCRIBE` 或
`EXPLAIN`，拒绝 DDL、DML、`COPY` 和多语句。

## 7. 快照与一致性

### 7.1 构建过程

一次 Catalog 构建按以下顺序完成：

```text
解析并校验挂载
  → 冻结每个根的候选成员
  → 固定每个候选的 identity / CURRENT / manifest / object metadata
  → 构造 sources 元数据
  → 计算 snapshot_id
  → 注册 Dataset schema、CatalogTableProvider 与默认 view
  → 发布给查询或 Server
```

只有完整构建成功的 `DatasetCatalogSnapshot` 才会交给查询引擎。构建过程不打开 Lance
dataset、不把远程 JSON 复制到本地，也不把 canonical events 规范化为 Storyline 三表。

### 7.2 不同源的固定方式

| 源 | 成员固定 | 内容/版本固定 |
|---|---|---|
| 本地外围文件 | 发现时冻结路径列表 | 记录路径、size、mtime，以及 Unix 上的 device/inode；命中后读取前后再次校验 |
| 远程外围对象 | 冻结 listing 的 `ObjectMeta` | 命中后按固定 version/ETag 条件读取，流式复制到快照临时目录，并校验最终大小 |
| Storyline store | 发现并读取 `CURRENT` 描述 | 冻结 generation 与三张表的精确版本；命中后才打开 Lance dataset |
| canonical events | 发现并读取 `_manifest.json` | 冻结 manifest revision 和可见 segment version；命中后才打开 segment |

只有被查询选中的远程对象才会复制。复制按 chunk 写入受快照持有的临时文件，不把整个
对象一次性读入内存；快照释放后临时目录随之清理。本地 fingerprint 是变化检测，不是
内容哈希：如果攻击者保留相同文件身份、大小和修改时间进行原地改写，不在保证范围内。

### 7.3 一致性边界

快照保证：

- 查询计划和执行看到相同的 source 成员集；
- Storyline/events 即使延迟打开，也只能打开快照已经固定的 generation、manifest 和
  segment version，不会重新读取最新指针；
- 后端提供 version/ETag 时，远程外围对象固定到 listing 时对应的版本；
- 新文件、新 generation 或新 manifest 只有下一条 CLI 查询或显式刷新后可见。

快照不保证多个彼此独立的 URI 来自同一个全局事务时刻，也不阻止源系统删除已经固定但
尚未读取的数据。本地文件在发现与首次读取之间发生可检测变化时，查询会失败而不是混读。
若对象后端既不提供 version 也不提供 ETag，Catalog 只能以 key、size 和修改时间描述
`snapshot_ref` 并校验传输大小，不能提供相同强度的对象版本固定保证。

`snapshot_id` 是 Dataset 名称、URI、格式提示、source 相对路径、固定引用与候选错误的
BLAKE3 摘要截断值，用于标识成员/版本视图；它不是内容校验和，也不代表业务提交 ID。

### 7.4 解析生命周期

快照中的每个 ready source 都持有一个固定描述和一个解析 cell。首次命中时：

```text
CatalogTableProvider source pruning
  → LazySource::resolve
  → 打开固定 Lance 版本，或校验/物化固定文件
  → 创建原生 TableProvider
  → 缓存 Result<ResolvedSource>
```

因此“惰性”不改变快照边界：解析发生得晚，但解析目标在 Catalog 发布前已经固定。未被任何
查询命中的 source 在整个快照生命周期中可以始终保持未打开状态。

## 8. 错误策略与资源边界

`--dataset-errors` 提供两种策略：

| 策略 | 单个候选无法固定描述或通过初始校验 | Dataset 根不存在、listing/遍历失败或超过全局限制 |
|---|---|---|
| `strict` | Catalog 构建失败 | Catalog 构建失败 |
| `report` | 写入 `<dataset>.sources`，状态为 `error`，跳过数据表注册 | Catalog 构建失败 |

`report` 的目标是容忍一个可信成员集中的坏文件，不是把不完整 listing 伪装成成功。候选
错误写入公开 Catalog 前会去掉错误文本中 URI query 部分，避免反射可能存在的临时签名；
生产配置仍不应把凭证直接放进 URI。延迟到 SQL 扫描期才出现的 Lance 打开、远程条件
读取、格式检测或记录解析错误在 `strict` 和 `report` 下都会让该查询失败，既不会静默
漏掉 ready source，也不会追溯修改不可变快照中的 `sources.status`。

Catalog 复用直接文件查询的资源参数：

- `max_files`：候选 source 数上限；
- `max_entries`：目录项或 object listing 数上限；
- `max_detection_bytes`：格式检测输入上限；
- `max_file_bytes`：外围文件/对象大小上限；
- `max_record_bytes`、`max_concurrent_files`、cache 参数：解析期边界；
- `max_concurrent_sources`：单次物理 scan 同时解析的 source 上限；
- `max_event_fallback_rows`、`max_event_fallback_bytes`：无 fresh 投影时单次定向
  canonical→Storyline fallback 的内存边界；
- DataFusion memory pool、spill path、spill bytes、timeout 与输出行数：查询期边界。

`--query-metrics` 只聚合已经解析的外围文件 source 的读取、裁剪、缓存和 buffer 指标；
未命中 source 不会为了生成指标而解析。`sources` 表用于解释“发现了什么”，metrics 用于
解释“实际扫描了什么”。

## 9. Server、刷新与 Web

`pchronicle serve` 从静态 Warehouse 配置挂载命名 Dataset。Catalog 在第一个需要数据的
请求到达时惰性构建，随后由所有 REST 和 SQL 请求共享。

| API | 语义 |
|---|---|
| `GET /api/catalog` | 返回当前 `snapshot_id`、创建时间、默认 Dataset、错误策略和 source 列表 |
| `POST /api/catalog` | 在锁外完整构建新快照，成功后原子替换，并清空轨迹缓存 |

刷新失败不会清空或部分更新旧 Catalog；正在处理的请求持有旧快照的 `Arc`，可以继续完成。
Web Explorer 从 Catalog 获取 Dataset 列表，服务端过滤、URL 状态和 Storyline 列表
均携带完整 `(dataset, _file_, session_id)`；`run_id` 作为物理 Run 分组信息
单独返回。Catalog 是不可变快照，新增数据只在显式 refresh 后进入 Web 视图。

### 9.1 Server source-routing 加速

Server 在每一代 `CatalogRuntime` 内持有可重建的内存加速结构，但不改变
`DatasetCatalogSnapshot`、`CatalogDataset` 或 `DiscoveredSource` 的定义。索引按需从当前
快照的稳定表派生：

- `runs` 索引保存 `run_id`、`session_id`、`agent_id`、`agent_model_name` 到 source id 的
  多值映射；
- `events` 使用两级 lazy index：identity 层保存 `event_id`、`trace_id`，partition 层保存
  `session_id`、`agent_id`；项目列表不会为高基数 event identity 付出内存成本；
- source 路径只在每个 Dataset 内保存一次，值键使用每代随机 keyed 64-bit fingerprint，单 source 命中
  内联保存整数 source id；hash collision 只会扩大候选集，原 SQL 谓词仍负责最终过滤；
- Run 列表另行惰性缓存，不让 SQL point query 为 Explorer 的 `row_count` 聚合付费。

索引只在首个包含可路由条件的单表查询到达时构建，并由 async single-flight 防止并发重复
扫描。构建使用 Arrow batch stream，不收集完整结果；单层索引最多接受 100 万行和 100 万
distinct value，超过边界即丢弃未发布的临时索引并回退原查询。Server 只从顶层 `AND` 中
提取必然成立的字符串等值或 `IN` 条件；联接、CTE、析取、
复杂表达式、已有 `_file_` 条件、过多候选 source 或索引构建失败都保留原 SQL。命中时只向
SQL 增加 `_file_ = ...` 或 `_file_ IN (...)`，原业务谓词仍由 DataFusion 执行。因此索引
只能缩小物理 source 候选，不能改变结果语义。

`GET /api/catalog` 的 `acceleration` 字段报告索引是否已经构建及其行、source、distinct
value 数，并通过 `failed` 列出本 generation 已缓存的构建失败，避免每个请求重复全表扫描；
`POST /api/query/evidence` 用 `source_routing` 响应字段报告 `applied`、
`already_pruned`、`not_applicable`、`not_selective` 或 `index_unavailable`。Catalog 刷新会把
新快照、查询引擎和空加速结构作为同一个 runtime 原子发布；旧请求继续持有旧 runtime，
索引不会跨 `snapshot_id` 复用。

首次索引构建仍需扫描对应稳定表，主要收益来自同一 Server 生命周期内的后续 point/project
查询。CLI 的一次性 SQL 不使用这层状态，也不会让 Catalog 变成持久化元数据服务。

### 9.2 写入边界

`pchronicle serve` 只提供读取、Catalog 刷新和有界 evidence query，不暴露 maintenance、
导入或任意 SQL 写接口。服务强制限制为 loopback；Gateway 和原生 writer
直接写 Dataset，不经过 Warehouse API。

## 10. Rust API 边界

核心 API 由 `persisting-pchronicle` 提供：

```rust
use std::sync::Arc;
use persisting_pchronicle::{
    CatalogSnapshotOptions, ChronicleQueryEngine, DatasetCatalogSnapshot, DatasetMount,
};

let mounts = vec![
    DatasetMount::new("current", "local:///srv/pchronicle/current")?,
    DatasetMount::new("archive", "s3://trajectory-bucket/archive")?,
];
let snapshot = Arc::new(
    DatasetCatalogSnapshot::discover(mounts, None, CatalogSnapshotOptions::default()).await?,
);
let engine = ChronicleQueryEngine::from_catalog_snapshot(snapshot).await?;
let rows = engine
    .query_jsonl("SELECT COUNT(*) AS runs FROM archive.runs")
    .await?;
```

需要按 Storyline 读取完整轨迹时使用 `CatalogStorylineKey` 调用快照的 `load_storyline`、`load_events`
或 `canonical_event_uri`。控制面可用 `list_namespaces`、`list_sources` 和 `describe_source`
分页浏览同一快照；page token 与 `snapshot_id` 绑定，刷新后不能复用。调用方不应绕过快照
重新发现 source，否则可能把不同成员或版本拼进同一个响应。

## 11. 关键不变量

实现和后续扩展必须保持以下不变量：

1. Namespace 是层级逻辑身份；SQL alias 是独立且唯一的小写 schema 名，二者不得混作一个字段。
2. `_file_` 在一个快照内稳定、相对 Dataset 根，根 source 固定表示为 `.`。
3. Catalog Storyline 的完整身份始终是 `(dataset, _file_, session_id)`；`run_id` 只用于 Run 分组。
4. 识别复合 store 后不得继续把其内部文件注册为独立 source。
5. 六张表即使为空也必须存在，并保持固定 schema。
6. `events` 只能包含 canonical events，不能由有损 Storyline 反向伪造。
7. 同 Dataset 多 source 的轨迹表联接必须携带 `_file_` 等值。
8. 查询期 source 不能脱离持有它的快照生命周期。
9. Server 只原子发布完整新快照；失败时继续提供旧快照。
10. Warehouse Server 不得把任何 Dataset 或 source 作为写目标。
11. `_file_` source pruning 必须发生在 `LazySource::resolve` 之前；不能为了判断是否命中而
    打开 source。
12. 延迟解析只能使用快照固定的版本描述，并在同一快照内 single-flight；不得在解析时
    重新跟随 `CURRENT` 或最新 manifest。
13. 多个投影关联同一 canonical events source 时必须保留 canonical source，并稳定选择最多
    一个 fresh 投影；冲突只能形成诊断信息，不能阻断事实读取。
14. Server routing index 必须与 snapshot 同代发布；构建或分析不确定时只能回退原查询，
    不得用不完整索引排除 source。

## 12. 取舍与备选方案

### 12.1 持久化元数据服务

持久化 Catalog 能缓存 listing 和统计信息，但会引入一致性协议、迁移、后台同步、权限与
灾难恢复问题。当前工作负载更需要“对这一条查询看到什么”的确定边界，因此选择查询期
快照。若未来 listing 成本成为主瓶颈，可以在不改变 SQL 模型的前提下增加可验证缓存。

### 12.2 把所有挂载平铺到 `public`

平铺表无法区分在线与归档边界，也会让 `_file_` 必须编码 URI 或 Dataset 名称。使用
DataFusion schema 保留用户给出的 Dataset 语义，并让跨 Dataset SQL 显式可审查。

### 12.3 用目录 basename 作为默认名称

basename 会受路径拼写、对象前缀和部署目录影响，不带 schema 的 SQL 也无法获得稳定解析结果。位置参数入口
因此始终使用固定名称 `dataset`，而不是从 URI 猜名字。

### 12.4 发现时统一导入 Lance

自动导入会改变查询的延迟、容量和失败语义，还会制造新的持久状态。Catalog 只做虚拟
规范化；重复大规模分析由用户显式转换为 Storyline Lance，以获得列裁剪、索引和版本发布。

### 12.5 只使用 `run_id`

`run_id` 是分组键，不是 Storyline 主键；同一物理 Run 内的主 Agent 与 subagent 可以共享
该值。用 `(dataset, _file_, session_id)` 定位既能保留物理来源，也能支持无歧义的读写路由。

## 13. 测试与演进

当前测试覆盖：

- Dataset 名称规范化、保留名与重名拒绝；
- 本地混合格式递归发现、默认 view 和空 Dataset schema；
- `strict`/`report` 候选错误行为；
- Catalog 和查询引擎构建后 source 解析计数仍为零；
- `_file_` 与业务谓词组合只解析命中的本地、远程和 Storyline source；
- 单 source 物理计划没有 `UnionExec`，未命中的远程对象不会下载；
- 延迟错误不会在 `report` 下被静默跳过；
- canonical `events` 原始扫描和 session 点查不会触发全量 Storyline 规范化；
- 多 fresh 投影稳定选出一个且 canonical events 始终可见；
- 层级 namespace 分页、source describe 和跨快照 page-token 拒绝；
- 同 Dataset 危险联接拒绝与跨 Dataset 联接；
- 一个 canonical events source 中多个 Storyline 的独立读取；
- CLI 位置参数、单/多命名挂载、TOML 与帮助文本；
- Server 惰性 Catalog、Dataset 过滤、失败刷新保留旧快照和物理写入坐标；
- Server routing index 的多条件交集、单 source SQL 注入、结果等价、显式 `_file_` 保留与
  refresh 后清空；
- Web Dataset 选择与完整 Run 坐标编码。

后续扩展新格式或新后端时，应先定义它如何产生稳定 `_file_`、如何固定版本、能投影哪些
表、是否允许写入，再接入发现器。不能固定成员或版本的后端必须显式降低一致性承诺，不能
复用现有 `snapshot_ref` 暗示更强保证。

## 14. 相关实现

- `crates/persisting-pchronicle/src/store/catalog/`：发现、固定、惰性 source、Catalog
  provider、source pruning 和 Run 路由；
- `crates/persisting-pchronicle/src/store/query_engine.rs`：Catalog DataFusion backend 与联接校验；
- `crates/persisting-pchronicle/src/store/storyline/datafusion.rs`：Storyline 描述固定与按固定
  generation 延迟打开；
- `crates/persisting-pchronicle/src/store/events/datafusion.rs`：canonical event manifest
  固定与按固定 segment 延迟打开；
- `crates/persisting-pchronicle-cli/src/lib.rs`：查询 CLI 挂载与默认 Dataset 解析；
- `crates/persisting-pchronicle-cli/src/server/mod.rs`：惰性构建、原子刷新、读写路由；
- `crates/persisting-pchronicle-cli/src/server/acceleration.rs`：同代内存 source-routing index、
  保守 SQL 分析与 `_file_` 注入；
- `pchronicle-web/src/`：Dataset 选择和完整 Run identity。
