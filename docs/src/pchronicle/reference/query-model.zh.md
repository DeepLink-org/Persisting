# Query Model 参考

每个挂载的 Dataset 都是一个 SQL schema。位置 Dataset 命名为 `dataset`；
`--mount NAME=DATASET` 创建命名 schema。即使没有兼容数据提供关系，也稳定暴露六个关系。

| 关系 | 一行表示什么 | 来源 |
| --- | --- | --- |
| `sources` | 一个发现候选 Source | 每个 Dataset |
| `runs` | 一个规范化 Run/session | 每个就绪的 Run 数据源 |
| `steps` | 一个规范化 Step | 每个就绪的 Run 数据源 |
| `tool_calls` | 一次工具调用及关联结果 | 每个就绪的 Run 数据源 |
| `events` | 一条 canonical 写入时事实 | 仅 canonical event Source |
| `trajectories` | 一个带有序 Step 与工具汇总的完整 Run | 规范化 Run 数据源 |

使用 `DESCRIBE` 查询已安装版本的精确 column：

```sql
DESCRIBE dataset.sources;
DESCRIBE dataset.runs;
DESCRIBE dataset.steps;
DESCRIBE dataset.tool_calls;
DESCRIBE dataset.events;
DESCRIBE dataset.trajectories;
```

## Source Identity

实体 ID 是 Source-local。`runs`、`steps`、`tool_calls` 与 `events` 保留 `_file_`，即
Dataset-relative `source_path`。持久实体地址包含 Dataset URI、`_file_`、entity kind 和
original ID。

在一个 Dataset 内联接内建轨迹关系时，entity key 必须同时带 `_file_`：

```sql
SELECT r.run_id, s.step_id, s.message_kind, s.message_value
FROM dataset.runs r
JOIN dataset.steps s
  ON r._file_ = s._file_
 AND r.session_id = s.session_id;
```

遗漏 `_file_` 的内建 join 会被拒绝，因为两个 Source 中相等的 ID 不代表同一实体。跨不同
命名 Dataset 时，每个 schema 已经是不同 namespace，不要求 `_file_` 相等。

## `sources`

| Column | 类型 | 含义 |
| --- | --- | --- |
| `_file_` | UTF-8, non-null | Dataset-relative Source path |
| `format` | UTF-8, nullable | 检测或声明的表示 |
| `kind` | UTF-8, non-null | `store` 或 `file` |
| `snapshot_ref` | UTF-8, nullable | generation、manifest revision、fingerprint、version 或 ETag |
| `projection_status` | UTF-8, nullable | canonical events Source 关联投影的 `fresh` 或 `stale` 状态 |
| `projection_generation` | UTF-8, nullable | 被选为读取加速投影的 generation |
| `projection_candidates` | UInt64, non-null | 参与选择的关联投影候选数 |
| `size_bytes` | UInt64, nullable | 候选文件或 marker object 大小 |
| `last_modified` | UTF-8, nullable | 可用时的 RFC 3339 timestamp |
| `status` | UTF-8, non-null | `ready` 或 `error` |
| `error` | UTF-8, nullable | 脱敏的 discovery 或 resolve 错误 |

外围文件被惰性打开前，`format` 可以保持 null。使用 `_file_` 过滤可以避免打开无关 Source。
`snapshot_ref` 只是展示投影；Rust/API 调用方应使用类型化的 `CatalogSourceRevision` 做一致性判断。

## Find 表达式

`pchronicle find --match` 是当前定位语法。已安装 CLI 的解析器（`FindExpr`）为准。
[RFC-0012](../../rfcs/0012-pchronicle-find-query-syntax.md) 是已接受的决策记录，不是命令参考。

普通词搜索已索引的 Storyline Step 内容（FTS / Jieba）。限定字段使用 `#field(term)`：

| 选择器 | 含义 |
| --- | --- |
| `#content` | `message_value`、`observation` 和 `prompt` |
| `#message` | `message_value` |
| `#user` | `source = 'user'` 的 `message_value` |
| `#assistant` | `source = 'agent'` 的 `message_value`（`#agent` 是别名） |
| `#system` | `source = 'system'` 的 `prompt` 与 `message_value` |
| `#reasoning` | `reasoning_content` |
| `#observation` | `observation` |
| `#prompt` | `prompt` 与 `message_value` |
| `#model` | `model_name`（`#model_name` 是别名） |
| `#env` | `env` |
| `#all` | 全部已索引的 Step 文本列 |

`AND` / `OR` / `NOT` 与括号组合谓词。JSONB 使用 `$.path OP value` 或
`#json.COLUMN("$.path") OP value`，`OP` 为 `=`、`!=`、`>`、`>=`、`<` 或 `<=`。
重复 `--match` 表示 AND。

当前实现按表达式推断 `search.scope`：

| 表达式 | `search.scope` | `search.mode` |
| --- | --- | --- |
| 仅文本 | `steps` | `fts` |
| 仅 JSON，未指定 Step 列 | `runs` | `json` |
| 仅 `#json.metrics(...)` | `steps` | `json` |
| 文本加 JSON | `steps` | `fts+json` |
| 仅身份标志 | 身份查找 | `identity` |

不含 `#json.metrics(...)` 的纯 JSON 表达式搜索 Run 级 JSONB 列
（`agent_extra`、`final_metrics`、`extra`、`meta`、`unknown_fields`）。
文本与 JSON 混合，以及显式 `#json.metrics(...)`，搜索 Step 级 JSONB
（`metrics`、`extra`）。

## Query 边界

引擎接受单条只读 `SELECT`、`VALUES`、`DESCRIBE` 或 `EXPLAIN`，拒绝 DDL、DML、`COPY`、
修改函数和多语句。CLI 的行数、字节、discovery 与 timeout 上限仍然生效。

Storyline 精确物理列见 [Storyline Lance](../design/storyline-lance.md)，discovery 与 predicate
pruning 机制属于 [Snapshot 设计](../design/catalog.md)，完整工作流见
[查询指南](../guides/discover-and-query.md)。
