# RFC-0011: Storyline 逻辑查询模型

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Schema / format name** | `logical/v1`（SQL 查询面）；物理层仍为 Storyline 三表 |
| **Date** | 2026-08-30 |
| **Component** | `persisting-pchronicle`（`store/catalog`、`store/storyline`、`formats/*`） |
| **Implements** | 尚未实现 |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0004 ACTF](0004-actf-format.md) · [Query Model 参考](../pchronicle/reference/query-model.md) · [Storyline Lance](../pchronicle/design/storyline-lance.md) |

---

## 摘要

本 RFC 在**不改变 Storyline 物理三表**的前提下，定义一层完整的逻辑查询模型。

物理三表（`runs` / `steps` / `tool_calls`）作为规范化投影是合格的。问题在于它们**同时**被
当成了查询接口，而其间没有任何逻辑层设计。当前的 `trajectories` 关系定义是
`SELECT r.*` 加八个相关子查询（`store/catalog/provider.rs:424`），也就是说它的 schema
等于"物理表有什么它就有什么"。这不是设计得不好，是没有设计。

本 RFC 定义 **6 张逻辑表**（3 个粒度 × 元数据/内容各一张）、**1 个键族**、**1 套结果语义
契约**，并逐列说明哪些可以由视图导出、哪些必须在写入期物化。

---

## 动机

### 现场证据

一次真实的 agent 使用报告（分析类任务，经 CLI + SQL）给出的主要障碍：

| 报告的现象 | 机械原因 |
|---|---|
| preview 被 system 提示和工具说明淹没；大块 JSON 造成噪声与截断 | `trajectories` 视图把 `messages_value`、`tool_results_json` 全文聚合成数组，只要投影没被剪掉就全量物化 |
| 个别查询触发内部错误 / stack overflow | 八个相关子查询嵌在视图里，外层再套一层视图（`store/catalog/mod.rs:701`），计划树过深 |
| `final_metrics` 是 LargeBinary，`json_extract` 不稳定 | `SELECT r.*` 把 `lance.json` 列裹进相关子查询的计划中 |
| 没有可靠的 `success` 字段，成败要人工读 `final_metrics` 判断 | `runs` 的 27 列里没有任何结果字段 |
| `step_count` / `tool_call_count` 要手工去重聚合 | 这两个数在每次查询时用相关子查询重算，而非写入期物化 |
| 从检索结果到 SQL 需要手工拼接 `_file_` / document / session / step | 实体 ID 是 Source-local，join 必须带 `_file_`，且没有单一轨迹键 |

### 物理产物泄漏清单

`SELECT r.*` 直接把以下存储记账列暴露给用户：`storage_ordinal`、
`trajectory_id_explicit`、`unknown_fields`、`unknown_key_counts`。`steps` 另有
`had_tool_calls`、`had_observation` 两个哨兵，`tool_calls` 有 `result`（nullable）与
`results`（non-null）的派生副本不对称。用户必须先分辨哪些是领域概念。

### 单厂商概念占据一等列

`steps.reasoning_effort_kind` / `reasoning_effort_value`（OpenAI API 参数）、`steps.ttft`、
`steps.is_copied_context`、`tool_calls.function_name`（OpenAI 命名）。逻辑层的职责之一
就是把这类概念收进开放容器，而不是让它们占据规范列名。

---

## 设计原则

1. **每张逻辑表声明唯一粒度。** 一行表示什么，必须能一句话说清，且表内不得混入其他粒度的
   聚合。当前 `trajectories` 粒度是轨迹却挂着 step 粒度数组，是全文被拖出的根因。
2. **唯一键族。** 每个粒度一个单列规范键，跨逻辑表 join 只用它。这消除 `_file_` 拼接。
3. **物理产物不出现在逻辑层。** 存储记账、哨兵、派生副本一律隐藏。
4. **内容与元数据分表。** 任何逻辑元数据表都不含大文本列，内容在同粒度的 `*_content`
   伴随表中，显式 join 才可见。
5. **语义字段一等化，且必须可信。** 结果、状态、时长是一等列；无法诚实断言时表达为
   "未知"，不得伪造。

---

## 键族

实体 ID 是 Source-local（见 [Query Model](../pchronicle/reference/query-model.md#source-identity)），
因此规范键必须把 `_file_` 编进键内。

| 键 | 粒度 | 构造 |
|---|---|---|
| `trajectory_key` | 轨迹 | `enc(_file_) # enc(document_id)` |
| `turn_key` | 步 | `trajectory_key @ turn_ordinal` |
| `tool_call_key` | 工具调用 | `trajectory_key @ turn_ordinal . call_index` |

`enc()` 为百分号编码，仅转义 `%`、`#`、`@`。键是可打印、可粘贴、可逆的，因此能直接出现在
bug 报告和 `WHERE` 子句里。`_file_` 与 `document_id` 仍作为普通列保留，以支持按路径前缀过滤。

**join 规则**：逻辑表之间 `USING (trajectory_key)` 是安全且被允许的，因为键已含 `_file_`。
针对物理表的 `_file_` 强制检查保持不变。

---

## 逻辑表

3 个粒度，每个粒度一张元数据表加一张内容表。`sources` 与 `events` 不在本 RFC 范围内，
保持现状。

### `trajectories`

> **粒度：一条轨迹（一个 Run/session 的一次 attempt）。** 不含任何大文本列。

**身份与溯源**

| 列 | 类型 | 说明 |
|---|---|---|
| `trajectory_key` | `Utf8` non-null | 规范键 |
| `_file_` | `Utf8` non-null | Dataset-relative source path |
| `document_id` | `Utf8` non-null | Source-local 文档 ID |
| `session_id` | `Utf8` non-null | |
| `run_id` | `Utf8` nullable | |
| `attempt_id` | `Utf8` nullable | |
| `task_id` | `Utf8` nullable | 短标识；完整任务描述在 `trajectory_content.task` |
| `origin` | `Utf8` nullable | |
| `source_schema_version` | `Utf8` non-null | 原 `runs.schema_version` |
| `agent_id` | `Utf8` non-null | |
| `agent_name` | `Utf8` nullable | |
| `agent_version` | `Utf8` nullable | |
| `model_name` | `Utf8` nullable | 原 `agent_model_name` |
| `parent_trajectory_key` | `Utf8` nullable | 续跑血缘；由 `runs.parent` 解析 |

**结果语义**

| 列 | 类型 | 说明 |
|---|---|---|
| `status` | `Utf8` non-null | 归一枚举，见[结果语义契约](#结果语义契约) |
| `success` | `Boolean` **nullable** | `null` 表示未知，**不是** `false` |
| `outcome_source` | `Utf8` non-null | 该判定的依据来源 |
| `score` | `Float64` nullable | |
| `error_kind` | `Utf8` nullable | 低基数归一值 |
| `error_message` | `Utf8` nullable | 截断到 `max_error_chars`；全文在 `trajectory_content` |
| `termination_reason` | `Utf8` nullable | `stop` / `max_turns` / `error` / `cancelled` / … |

**规模与成本**（全部写入期物化，不得为子查询）

| 列 | 类型 | 说明 |
|---|---|---|
| `turn_count` | `Int64` non-null | |
| `tool_call_count` | `Int64` non-null | |
| `tool_error_count` | `Int64` non-null | |
| `distinct_tool_count` | `Int64` non-null | |
| `started_at` / `finished_at` | `Timestamp(ns, UTC)` nullable | |
| `duration_ms` | `Int64` nullable | |
| `input_tokens` / `output_tokens` / `cached_tokens` / `total_tokens` | `Int64` nullable | |
| `cost_total` | `Float64` nullable | |
| `llm_ms_total` / `tool_ms_total` | `Int64` nullable | |
| `content_bytes` | `Int64` non-null | 该轨迹全部内容字节，用于预判钻取代价 |

**开放扩展**

| 列 | 类型 | 说明 |
|---|---|---|
| `metrics` | `lance.json` nullable | `final_metrics` 中未被提升为列的部分 |
| `labels` | `Map<Utf8, Utf8>` | benchmark / owner / experiment 等维度，开放 |
| `attrs` | `Map<Utf8, Utf8>` | 单来源专属的低基数标量 |

**不出现在本表**：`storage_ordinal`、`trajectory_id_explicit`、`unknown_fields`、
`unknown_key_counts`，以及任何 step 粒度的聚合数组。

### `trajectory_content`

> **粒度：一条轨迹。** 承载轨迹级大文本，显式 join 才读。

| 列 | 类型 | 说明 |
|---|---|---|
| `trajectory_key` | `Utf8` non-null | |
| `task` | `Utf8` nullable | 完整任务描述 |
| `prompt` | `Utf8` nullable | |
| `tool_definitions` | `lance.json` nullable | 原 `agent_tool_definitions` |
| `notes` | `Utf8` nullable | |
| `error_full` | `Utf8` nullable | 未截断错误全文 |
| `extra` | `lance.json` nullable | |
| `meta` | `lance.json` nullable | **已脱敏**（见[凭证](#凭证)） |

### `turns`

> **粒度：一步。** 不含任何大文本列，只有尺寸与存在性。

| 列 | 类型 | 说明 |
|---|---|---|
| `turn_key` | `Utf8` non-null | 规范键 |
| `trajectory_key` | `Utf8` non-null | → `trajectories` |
| `turn_ordinal` | `Int64` non-null | |
| `step_id` | `Int64` non-null | Source-local 步 ID |
| `parent_turn_ordinal` | `Int64` nullable | `null` 即线性；非 null 表示分支（**需物理层新增**） |
| `actor` | `Utf8` non-null | 归一：`user` / `model` / `tool` / `system` / `env` / `other` |
| `actor_raw` | `Utf8` nullable | 原 `steps.source` |
| `kind` | `Utf8` non-null | 归一，原 `steps.effective_kind` |
| `kind_raw` | `Utf8` nullable | 原 `steps.kind` |
| `model_name` | `Utf8` nullable | |
| `stop_reason` | `Utf8` nullable | 从 `metrics` 提升 |
| `timestamp` / `finished_at` | `Timestamp(ns, UTC)` nullable | |
| `latency_ms` | `Int64` nullable | 原 `steps.latency` |
| `ttft_ms` | `Int64` nullable | 原 `steps.ttft` |
| `llm_call_count` | `Int64` nullable | |
| `input_tokens` / `output_tokens` | `Int64` nullable | 从 `metrics` 提升（**需物理层新增**） |
| `tool_call_count` | `Int64` non-null | 物化 rollup |
| `has_error` | `Boolean` non-null | 本步任一工具调用报错 |
| `text_bytes` | `Int64` non-null | |
| `reasoning_bytes` | `Int64` non-null | |
| `observation_bytes` | `Int64` non-null | |
| `metrics` | `lance.json` nullable | |
| `attrs` | `Map<Utf8, Utf8>` | 收纳 `reasoning_effort_*`、`is_copied_context`、`env` 等 |

`has_reasoning` / `has_observation` 不设列——用 `reasoning_bytes > 0` 表达即可，避免再引入
一组可能与内容不一致的哨兵。物理层的 `had_tool_calls` / `had_observation` 不暴露。

### `turn_content`

> **粒度：一步。** 承载步级大文本。

| 列 | 类型 | 说明 |
|---|---|---|
| `turn_key` | `Utf8` non-null | |
| `trajectory_key` | `Utf8` non-null | |
| `turn_ordinal` | `Int64` non-null | |
| `content_kind` | `Utf8` non-null | 原 `steps.message_kind`，说明 `text` 的编码形态 |
| `text` | `Utf8` nullable | 原 `steps.message_value` |
| `reasoning` | `Utf8` nullable | 原 `steps.reasoning_content` |
| `observation` | `Utf8` nullable | |
| `extra` | `lance.json` nullable | |

### `tool_calls`

> **粒度：一次工具调用。** 不含调用结果正文。

| 列 | 类型 | 说明 |
|---|---|---|
| `tool_call_key` | `Utf8` non-null | 规范键 |
| `turn_key` | `Utf8` non-null | → `turns` |
| `trajectory_key` | `Utf8` non-null | → `trajectories` |
| `turn_ordinal` | `Int64` non-null | |
| `call_index` | `Int64` non-null | |
| `tool_call_id` | `Utf8` non-null | Source-local 调用 ID |
| `tool_name` | `Utf8` non-null | 原 `function_name` |
| `arguments` | `lance.json` non-null | 高价值查询目标，建 `$.command` / `$.path` 路径索引 |
| `is_error` | `Boolean` non-null | 物化 |
| `duration_ms` | `Int64` nullable | |
| `result_bytes` | `Int64` non-null | |
| `attrs` | `Map<Utf8, Utf8>` | 收纳 `tool_calls.kind` 等 |

### `tool_results`

> **粒度：一次工具调用。** 承载结果正文。

| 列 | 类型 | 说明 |
|---|---|---|
| `tool_call_key` | `Utf8` non-null | |
| `trajectory_key` | `Utf8` non-null | |
| `turn_ordinal` / `call_index` | `Int64` non-null | |
| `result` | `Utf8` nullable | **单一**结果视图；物理层 `result` / `results` 的不对称在此消除 |
| `response` | `lance.json` nullable | |
| `extra` | `lance.json` nullable | |

---

## 结果语义契约

这是整个逻辑层里唯一不能由 SQL 推导的部分，必须由格式适配器提供。

### `status` 枚举

| 值 | 含义 |
|---|---|
| `succeeded` | 任务达成，有明确依据 |
| `failed` | 任务未达成，有明确依据 |
| `errored` | harness / 基础设施错误，任务结果无意义 |
| `truncated` | 达到 turn / token / 时间上限而中止 |
| `incomplete` | 会话未正常结束且无更具体信息 |
| `unknown` | 无任何可信依据 |

### `outcome_source` 枚举

| 值 | 含义 |
|---|---|
| `harness_field` | 来源自带明确成败字段（如 `correct`、`resolved`） |
| `final_metrics` | 从 `final_metrics` 的已知路径提取 |
| `exit_code` | 从进程退出码推断 |
| `inferred_from_termination` | 仅从终止原因推断 |
| `absent` | 来源没有提供任何结果信息 |

### 为什么 `success` 必须可空

**把"未知"填成 `false` 会让系统里每一个成功率数字静默地错，这比没有这个字段更糟。**
`outcome_source` 的作用是让分析者能自己判断这批数能不能信：

```sql
-- 只在依据可靠的子集上算成功率，并暴露覆盖率
SELECT model_name,
       count(*) FILTER (WHERE success) * 1.0
         / nullif(count(*) FILTER (WHERE success IS NOT NULL), 0) AS success_rate,
       count(*) FILTER (WHERE success IS NULL) * 1.0 / count(*)   AS unknown_ratio
FROM trajectories
GROUP BY model_name;
```

### 适配器契约

每个 `TrajectoryFormat` **MUST** 实现结果映射，返回

```rust
pub struct Outcome {
    pub status: TrajectoryStatus,
    pub success: Option<bool>,
    pub score: Option<f64>,
    pub error_kind: Option<String>,
    pub error_message: Option<String>,
    pub termination_reason: Option<String>,
    pub source: OutcomeSource,
}
```

或显式返回 `source: OutcomeSource::Absent` 且 `status: Unknown`、`success: None`。
**不允许**适配器猜测。新增格式时如果不实现映射，默认落到 `Absent` / `Unknown`，
逻辑层依然可用，只是该来源的成功率计入 `unknown_ratio`。

---

## 实现分类：视图导出 vs 写入期物化

逻辑层只能投影、改名、隐藏、拆分。任何**语义**或**聚合**都必须物化，否则又回到相关子查询。

| 类别 | 实现 | 示例 |
|---|---|---|
| 改名 | 视图 | `function_name` → `tool_name`，`agent_model_name` → `model_name` |
| 隐藏 | 视图 | 剔除 `storage_ordinal`、`unknown_key_counts`、`had_*` |
| 拆分 | 视图 | `turns` / `turn_content` 从同一物理 `steps` 投影 |
| 收纳 | 视图 | `reasoning_effort_*`、`env`、`kind` 进 `attrs` |
| 键派生 | 视图 | `trajectory_key` 由 `_file_` + `document_id` 拼接 |
| 单位换算 | 视图 | `latency` → `latency_ms` |
| **聚合 rollup** | **物化** | `turn_count`、`tool_call_count`、`*_tokens`、`*_bytes`、`duration_ms` |
| **归一语义** | **物化** | `status`、`success`、`outcome_source`、`actor`、`error_kind` |
| **拓扑** | **物化** | `parent_turn_ordinal` |

视图部分零成本；物化部分利用 Lance 的 data evolution 加列，不重写既有数据。

### 需要的物理层新增列

| 物理表 | 新增列 | 来源 |
|---|---|---|
| `runs` | `status`、`success`、`outcome_source`、`score`、`error_kind`、`termination_reason` | 适配器结果映射 |
| `runs` | `turn_count`、`tool_call_count`、`tool_error_count`、`distinct_tool_count`、`content_bytes`、token 与耗时汇总 | 写入期计算 |
| `steps` | `parent_turn_ordinal` | 来源拓扑（事件流族的 `parentId`） |
| `steps` | `input_tokens`、`output_tokens`、`stop_reason`、`tool_call_count`、`has_error`、`text_bytes`、`reasoning_bytes`、`observation_bytes` | 写入期计算 / 从 `metrics` 提升 |
| `tool_calls` | `is_error`、`result_bytes` | 写入期计算 |

`steps.parent_turn_ordinal` 一旦引入，`reconstruct_storyline` 中
"turn ordinals must be contiguous from zero"（`store/storyline/model.rs:323`）需放宽为
唯一性 + 可达性校验，否则带分支的来源（Claude Code / OpenClaw 风格事件流）仍无法落库。

---

## 命名与迁移

**决策（可推翻）**：逻辑表占用普通名字，物理三表加前导下划线退居二线，与既有 `_file_` 的
系统列约定一致。

| 现在 | 之后 |
|---|---|
| `runs` | `_runs` |
| `steps` | `_steps` |
| `tool_calls` | `_tool_calls`（逻辑 `tool_calls` 语义不同，见上） |
| `trajectories`（视图） | 删除，由逻辑 `trajectories` 取代 |

这是破坏性变更。迁移路径：一个发布周期内保留 `runs` / `steps` 为 `_runs` / `_steps` 的
别名并在 `DESCRIBE` 输出中标注 deprecated；`trajectories` 因语义变化不设别名，直接切换并
在 release note 中列出列级差异。

`DESCRIBE` 仍是版本内精确列的唯一权威来源。

---

## 查询路径对比

**成功率与平均长度**（报告中"最不够一步到位"的场景）

```sql
-- 之后
SELECT model_name, count(*) AS n,
       avg(turn_count) AS avg_turns,
       count(*) FILTER (WHERE success) * 1.0
         / nullif(count(*) FILTER (WHERE success IS NOT NULL), 0) AS success_rate
FROM trajectories
GROUP BY model_name;
```

只碰 `trajectories` 一张表，无 join、无子查询、无内容读取。

**工具错误率**

```sql
SELECT tool_name, count(*) AS calls,
       count(*) FILTER (WHERE is_error) * 1.0 / count(*) AS error_rate
FROM tool_calls
GROUP BY tool_name
ORDER BY calls DESC;
```

**从检索结果钻取到内容**

```sql
-- 检索返回 turn_key，直接可用
SELECT c.text
FROM turn_content c
WHERE c.turn_key = 'runs%2Fa.lance#doc-1@42';
```

不再需要拼接 `_file_` / `document_id` / `session_id` / `step_id`。

**命令级下钻**（走 JSON 路径索引）

```sql
SELECT trajectory_key, turn_ordinal
FROM tool_calls
WHERE tool_name = 'Bash'
  AND json_get_string(arguments, 'command') LIKE '%pyomo%';
```

---

## 凭证

`trajectory_content.meta` 对应的来源字段中实测存在明文凭证（`plan.environment.params.secret_key`、
`plan.execution.analysis_params.*.api_key`）。逻辑层暴露该列前 **MUST** 完成键名模式脱敏
（`secret` / `token` / `api_key` / `password` / `credential` / `authorization`），
值替换为 `«redacted:<blake3-8>»`，保留可比较性而不保留明文。

---

## 未决问题

1. **命名迁移**是否接受破坏性变更，或需要更长的双名周期。
2. **粒度集合**是否完整。当前按发现、聚合、工具行为、单轨迹钻取、检索衔接五类消费者切分；
   训练数据导出是否需要独立粒度（一行一个可用样本）待定。
3. **`labels` 的来源**。benchmark / owner / experiment 这类维度在当前物理层没有归宿，
   需要确定它们来自 Source 路径解析、挂载配置，还是 `meta`。
4. **`content_bytes` 的口径**：是否计入 `tool_results`，以及是否按原始还是压缩后计。
5. **`arguments` 从 `Utf8` 改为 `lance.json`** 是物理列类型变更，无法用 data evolution
   原地完成，需要一次 generation 迁移；是否与本 RFC 同批推进待定。
