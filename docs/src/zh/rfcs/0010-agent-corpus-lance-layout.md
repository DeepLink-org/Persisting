# RFC-0010: Agent 评测语料的分层 Lance 布局

| Field | Value |
|---|---|
| **Status** | Proposed |
| **Schema / format name** | `corpus/v1`（逻辑）；物理为 `corpus/*.lance` 一组 dataset |
| **Date** | 2026-08-29 |
| **Component** | `persisting-pchronicle` |
| **Implements** | 尚未实现；本 RFC 定义目标布局 |
| **Related** | [RFC-0001 Storyline](0001-storyline-format.md) · [RFC-0004 ACTF](0004-actf-format.md) · [RFC-0009 OpenAI Messages](0009-openai-messages-format.md) · [Storyline Lance](../pchronicle/design/storyline-lance.md) |

---

## 摘要

本 RFC 定义一个**四层 + 两条旁路**的 Lance 布局，用于存放 Agent 评测语料（benchmark
harness 产出的 attempt 级 JSON）。

核心判断来自对一份 1562MB / 18 文件真实语料的实测：**99% 的体积是冗余和衍生物，真正的
信息量在 20MB 量级**。因此布局的第一原则不是"如何优雅地建模轨迹"，而是**按访问频率和体积
把数据切成物理隔离的层**，让日常查询永远不接触取证层的字节。

与既有 Storyline 三表（[storyline-lance](../pchronicle/design/storyline-lance.md)）的关系：
本布局是**入库侧的语料层**，负责无损收敛外部 harness 的异构输出；Storyline 仍是互操作枢纽。
两者共享 CAS 内容层的设计思路，但本布局的原子粒度更细（block 而非 step），并且显式建模
了 Storyline 没有的维度：多 attempt、analyzer 判定、取证产物、RL 偏好对。

---

## 动机

### 实测数据画像

语料路径约定为
`<owner>/<experiment>/<benchmark>/<model-tag>/<run>_<run-id>/details/<task-id>.json`，
一个文件对应一个 task 的一组 attempt。实测结论：

| 观察 | 实测值 |
|---|---|
| 文件级完全重复 | 1562MB → 892MB（18 文件中仅 15 个内容不同）；两个 654MB 文件 md5 相同 |
| 取证转储占比 | `attempts.1.artifacts.raw_researchharness_trace_events` = 651.1MB（单文件 99.5%），同文件规范轨迹仅 6.7MB |
| 同文件内逐字节重复 | `attempts.1.error`（3.06MB 单字符串）完整包含 `extra.harness_metrics.stdout`（3.06MB） |
| 二次型前缀增长 | RL 族每行携带完整历史，第 N 步 messages 是第 N+1 步的精确前缀；单 session 放大 25.6x |
| 逐步重复 | ACTF 族 `system_prompt` / `tools` 每步重存；1000 步文件内 tool schema 重复 8000 份 |
| 端到端可压缩性 | 205.9MB 内容字符串 → 去重 92.1MB（2.2x）→ 压缩 14.95MB（合计 14x） |

### schema_version 不可信

8 个 `trajectory` 为 dict 的文件**全部**声明 `schema_version: "ACTF_v1.0"`，但
`observation` 元素有四种互不兼容的形状：

1. `{content, is_error, tool_use_id, type}`（Claude 风格）
2. `{content}`
3. `{role, text, tool_names, error}`
4. 元素本身是 list（嵌套一层）

单文件出现 198 个不同键名，`retry_count` / `retry_counts` 只在部分文件出现，
`compaction` 事件只在部分文件出现。**入库必须按形状探测而非按声明版本分派，且必须有
无损兜底通道。**

### 三个来源族

| 族 | 形态 | 来源 |
|---|---|---|
| `actf_steps` | `trajectory = {schema_version, started_at, finished_at, steps[]}`；step 含 `step_id / system_prompt / tools / user_content / assistant_content / observation / metric / started_at / finished_at` | terminal_bench、swebench_pro、researchclawbench |
| `event_stream` | `trajectory = [{type, id, parentId, timestamp, ...}]`；类型 `session / model_change / thinking_level_change / custom / message / compaction`；message 角色 `user / assistant / toolResult` | skillsbench、pinchbench |
| `rl_rows` | 顶层即 list；行含 `session_id, step_id, messages[], chosen_response, rejected_response, reward, step_reward, is_trainable, is_terminal, is_truncated, blob_manifest, ...` | cybergym |

`event_stream` 族的 `parentId` 意味着轨迹是**树**而非线性序列。

---

## 分层模型

四个存储层，按"访问频率 × 体积"划分。层之间只通过键引用，物理上是独立 Lance dataset，
可以独立压缩、独立维护、独立过期。

```text
L0  溯源层   corpus_files ──< file_paths
              │  （物理文件身份；一个内容可有多个观测路径）
              ▼
L1  分析层   attempts ──< analysis
              │  （查询主脊：约 0.1% 字节，承载全部 triage 查询）
              ▼
L2  结构层   steps ──< blocks
              │  （有序骨架 + 内容块索引；窄表，全列可字典编码）
              ▼
L3  内容层   content            （CAS，按 digest 唯一，Blob v2 承载 payload）
              ▲        ▲
              │        │
L4  冷层     artifacts │        （取证转储：约 95% 字节，默认不进查询路径）
                       │
旁路 A       preferences        （RL 偏好对：派生训练视图）
旁路 B       tool_defs          （工具定义的去重投影）
```

分层的实际含义：

| 层 | 字节占比 | 访问频率 | 是否可丢 |
|---|---|---|---|
| L0 溯源 | ~0% | 中（血缘追溯） | 否 |
| L1 分析 | ~0.1% | **极高**（每次 triage） | 否 |
| L2 结构 | ~1% | 中（回放、导出） | 否，可从 L0 重建 |
| L3 内容 | ~4% | 中低（回放、导出） | 否，可从 L0 重建 |
| L4 冷层 | **~95%** | 极低（取证） | **是**，是 L2+L3 的衍生重复品 |

---

## L0 溯源层

### `corpus_files`

一行一个**内容不同**的物理文件。身份是内容而非路径，因此字节相同的文件天然合并。

| 列 | 类型 | 说明 |
|---|---|---|
| `file_digest` | `FixedSizeBinary(32)` | BLAKE3-256，主键 |
| `n_bytes` | `Int64` | 原始字节数 |
| `source_family` | `Utf8`（字典） | `actf_steps` / `event_stream` / `rl_rows` |
| `declared_schema_version` | `Utf8` nullable | 原样保留，**不用于分派** |
| `shape_fingerprint` | `FixedSizeBinary(32)` | 探测得到的形状指纹，用于分组同构文件 |
| `ingested_at` | `Timestamp(us, UTC)` | |
| `raw_digest` | `FixedSizeBinary(32)` | 指向 `content`，可选保留原始 JSON |

### `file_paths`

一行一个**观测到的路径**。同一 `file_digest` 可有多行——实测中两个 654MB 文件字节相同但
路径不同（`_2054` 与 `_clone_2081`），这是唯一诚实的表达方式：内容是一份，路径是两个观测。

| 列 | 类型 | 说明 |
|---|---|---|
| `file_digest` | `FixedSizeBinary(32)` | → `corpus_files` |
| `path` | `Utf8` | 相对语料根的完整路径 |
| `owner` | `Utf8`（字典） | 路径第 1 段 |
| `experiment` | `Utf8`（字典） | 路径第 2 段 |
| `benchmark` | `Utf8`（字典） | 路径第 3 段 |
| `model_tag` | `Utf8`（字典） | 路径第 4 段 |
| `run_name` | `Utf8`（字典） | 路径第 5 段去掉尾部 `_<id>` |
| `run_id` | `Int64` nullable | 路径第 5 段尾部数字 |
| `is_clone` | `Boolean` | `run_name` 含 `_clone` |
| `task_slug` | `Utf8` | 文件名去掉 `_error_` 前缀与 `.json` |
| `filename_marks_error` | `Boolean` | 文件名带 `_error_` 前缀 |
| `observed_mtime` | `Timestamp(us, UTC)` nullable | |

路径维度是**每一个查询的入口**（"benchmark=X 且 model=Y 的失败 attempt"），因此必须是
一等列而非从路径字符串现场解析。

---

## L1 分析层

### `attempts`

一行一个 attempt。这是查询主脊：**所有 triage 查询必须只靠这张表完成，不读任何内容字节。**

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | `blake3(file_digest ‖ attempt_no)`，主键 |
| `file_digest` | `FixedSizeBinary(32)` | → `corpus_files` |
| `attempt_no` | `Int32` | `attempts` 的键（`"1"`, `"2"`…） |
| `task_id` | `Utf8` | |
| `category` | `Utf8`（字典） | |
| `k` | `Int32` | |
| `attempts_tried` | `Int32` | |
| `retry_count` | `Int32` nullable | 仅部分来源有 |
| `correct` | `Boolean` | |
| `score` | `Float64` nullable | |
| `status` | `Utf8`（字典） | |
| `solved_at` | `Timestamp(us, UTC)` nullable | |
| `started_at` / `finished_at` | `Timestamp(us, UTC)` nullable | |
| `owner` … `filename_marks_error` | 同 `file_paths` | **反规范化**的规范路径维度（取该 digest 路径集的字典序最小者） |
| `n_steps` | `Int32` | 以下均为入库期预计算的 rollup |
| `n_messages` | `Int32` | |
| `n_blocks` | `Int32` | |
| `n_tool_calls` | `Int32` | |
| `n_tool_errors` | `Int32` | |
| `input_tokens` / `output_tokens` / `cache_read_tokens` / `cache_write_tokens` | `Int64` nullable | |
| `cost_total` | `Float64` nullable | |
| `llm_infer_ms_total` / `tool_exec_ms_total` / `wall_ms` | `Float64` nullable | |
| `has_error` | `Boolean` | |
| `error_digest` | `FixedSizeBinary(32)` nullable | → `content`；巨型 error 字符串不进本表 |
| `final_answer_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `ground_truth_json` | `lance.json` nullable | 形状不一（有时 str，有时 `{checklist_path}`） |
| `meta_json` | `lance.json` nullable | **已脱敏**的 `meta`，见"凭证脱敏" |
| `unknown_json` | `lance.json` nullable | 无损兜底 |

rollup 列是本设计成立的关键：它们把"筛出高成本 / 高延迟 / 工具报错多的 attempt"从一次
内容扫描降级为一次窄列扫描。

### `analysis`

一行一个 `(attempt, analyzer)`。**长格式**，不是 21 个固定列也不是一个 JSON blob。

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | → `attempts` |
| `analyzer` | `Utf8`（字典） | 实测 21 种，集合在增长 |
| `is_badcase` | `Boolean` nullable | 实测 188 次出现 |
| `score` | `Float64` nullable | 实测 118 次出现 |
| `analyzer_error` | `Utf8` nullable | 实测 55 次出现 |
| `details_digest` | `FixedSizeBinary(32)` nullable | → `content`；`ExceptionAnalyzer.details` 实测达 1.93MB |
| `details_inline_json` | `lance.json` nullable | details 小于阈值时就地存放，可直接查询 |

选长格式的三个理由：analyzer 集合在增长（新增不动 schema）；`details`/`score`/`error`
三个子键都是可选的（宽表会产生大量 null 列）；最常见的查询是"任一 analyzer 命中"，
长格式下是一次普通过滤而非 21 列 OR。

---

## L2 结构层

### `steps`

有序的回合骨架。`metric` / `usage` **完全摊平**成真实列——它们是聚合查询的主要目标，
留在 JSON 里等于放弃列式优势。

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | → `attempts` |
| `step_ordinal` | `Int32` | 从 0 连续，本布局赋予的规范序 |
| `source_step_id` | `Utf8` nullable | 来源自带的 `step_id` / 事件 `id` |
| `parent_step_ordinal` | `Int32` nullable | `event_stream` 族 `parentId` 解析结果；线性时为 null |
| `session_id` | `Utf8` nullable | `rl_rows` 族与 `session` 事件 |
| `model` / `provider` / `api` | `Utf8`（字典） nullable | |
| `stop_reason` / `finish_reason` | `Utf8`（字典） nullable | |
| `prompt_tokens` / `completion_tokens` | `Int64` nullable | ACTF `metric.{prompt,completion}_tokens_len` |
| `cache_read_tokens` / `cache_write_tokens` / `total_tokens` | `Int64` nullable | `event_stream` 族 `usage.*` |
| `cost_input` / `cost_output` / `cost_cache_read` / `cost_cache_write` / `cost_total` | `Float64` nullable | `usage.cost.*` |
| `llm_infer_ms` | `Float64` nullable | ACTF `metric.llm_infer_ms` |
| `env_action_ms` | `Float64` nullable | ACTF `metric.env_action_ms` |
| `started_at` / `finished_at` | `Timestamp(us, UTC)` nullable | |
| `system_prompt_digest` | `FixedSizeBinary(32)` nullable | → `content`；逐步重复由 CAS 吸收 |
| `tools_digest` | `FixedSizeBinary(32)` nullable | → `content` / `tool_defs` |
| `unknown_json` | `lance.json` nullable | |

### `blocks`

**本布局的核心表。** 原子是内容块，不是 message 也不是 step。三个来源族在这个粒度上完全
同构：ACTF 的 `assistant_content.{content, reasoning_content, tool_calls[]}`、
`event_stream` 的 `[thinking, text, toolCall]` 块列表、`rl_rows` 的 `messages[].content[]`
都落成同一组行。

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | → `attempts` |
| `step_ordinal` | `Int32` | → `steps` |
| `msg_ordinal` | `Int32` | 步内消息序 |
| `block_ordinal` | `Int32` | 消息内块序 |
| `role` | `Utf8`（字典） | `system` / `user` / `assistant` / `tool` / `runtime` |
| `block_type` | `Utf8`（字典） | `text` / `thinking` / `tool_call` / `tool_result` |
| `tool_name` | `Utf8`（字典） nullable | |
| `tool_call_id` | `Utf8` nullable | 关联 `tool_call` 与 `tool_result` |
| `is_error` | `Boolean` nullable | |
| `content_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `n_bytes` | `Int32` | 内容原始字节数 |
| `arguments_json` | `lance.json` nullable | tool call 参数；小且是高频查询目标 |
| `thinking_signature` | `Utf8` nullable | |
| `unknown_json` | `lance.json` nullable | |

`blocks` 的所有过滤列都是低基数可字典编码的，payload 只是定长摘要，因此
`WHERE tool_name = 'Bash' AND is_error` 这类查询扫描代价接近于零。

物理写入按 `(attempt_uid, step_ordinal, msg_ordinal, block_ordinal)` 排序，
`attempt_uid` 上建 BTree 标量索引：筛一批 attempt 走索引，回放一个 attempt 是一次连续
范围读。

---

## L3 内容层

### `content`

CAS。按 `digest` 唯一，是全布局唯一存放大段字节的地方。

| 列 | 类型 | 说明 |
|---|---|---|
| `digest` | `FixedSizeBinary(32)` | BLAKE3-256 of 未压缩原始字节，主键 |
| `n_bytes` | `Int64` | 未压缩长度 |
| `encoding` | `Utf8`（字典） | `raw` / `zstd` |
| `content_type` | `Utf8`（字典） | `text/plain` / `application/json` / … |
| `payload_inline` | `Binary` nullable | 小于 `inline_threshold` 时就地存放 |
| `payload_blob` | Lance Blob v2 | 大于阈值时走 blob，不投影则零字节读取 |

**去重与卸载解耦，这是与现行 Storyline 内容层最重要的分歧。** 摘要**总是**计算，去重在
`content` 表这一级无条件发生；`inline` 还是 `blob` 是 `content` 表内部的独立决定
（建议阈值 8KB）。现行设计用单一的 64KB `DEFAULT_CONTENT_OFFLOAD_THRESHOLD` 同时决定
这两件事，导致本语料的主要重复模式（小对象的成千上万次重复）完全逃逸。

需要说明清楚 CAS 相对列压缩的**边界**：同一 fragment 内的重复值，Lance 的字典编码和页
压缩本来就能吃掉，这部分不是 CAS 的功劳。CAS 不可替代的是两种情形——

1. **跨 dataset / 跨 run 的重复**：上千次 run 共享同一份 system prompt 与 tool schema，
   页压缩看不到跨文件边界；实测两个 654MB 文件完全相同，压缩无能为力。
2. **二次型前缀增长**：`rl_rows` 族第 N 步与第 N+1 步的 `messages` 是**不同的值**，字典
   编码失效；只有结构化引用能把 25.6x 放大压回 1x。

### `tool_defs`（旁路 B）

`tools_digest` 指向的工具定义列表的去重投影，让"哪些 run 可用工具 X"变成关系查询。
实测单文件内 tool schema 重复 8000 份，去重后是个位数行。

| 列 | 类型 | 说明 |
|---|---|---|
| `tools_digest` | `FixedSizeBinary(32)` | → `content` |
| `tool_name` | `Utf8`（字典） | |
| `description_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `parameters_json` | `lance.json` nullable | |

---

## L4 冷层

### `artifacts`

取证转储。**独立 dataset、独立生命周期、默认不进查询路径。**

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | → `attempts` |
| `artifact_name` | `Utf8`（字典） | `raw_researchharness_trace_events` / `harness_metrics.stdout` / `error` / … |
| `digest` | `FixedSizeBinary(32)` | → `content` |
| `n_bytes` | `Int64` | |
| `content_type` | `Utf8`（字典） | |
| `derives_from` | `Utf8`（字典） nullable | 标注它是 L2+L3 的衍生重复品时填 `trajectory` |

这一层承载实测 95% 的字节，却是 L2+L3 的衍生重复品——`error` 完整包含
`harness_metrics.stdout`，`raw_researchharness_trace_events` 是 `trajectory.steps` 的
逐次全量重放。因此它必须能被独立地过期、独立地丢弃，而不影响任何分析查询。
`derives_from` 让"可安全丢弃"成为可判定的属性。

### `preferences`（旁路 A）

`rl_rows` 族的偏好对。它是**派生的训练视图**，不是轨迹；硬塞进轨迹模型正是 25.6x 放大的
成因，所以单独一张表。

| 列 | 类型 | 说明 |
|---|---|---|
| `attempt_uid` | `FixedSizeBinary(32)` | → `attempts` |
| `session_id` | `Utf8` | |
| `step_ordinal` | `Int32` | → `steps`；**状态就是该步的 block 前缀，不再复制** |
| `chosen_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `rejected_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `response_digest` | `FixedSizeBinary(32)` nullable | → `content` |
| `reward` / `step_reward` | `Float64` nullable | |
| `is_trainable` / `is_terminal` / `is_truncated` / `is_session_completed` | `Boolean` | |
| `dataset_type` | `Utf8`（字典） | `TRAIN` / `TEST` / … |
| `dt` | `Date32` | |
| `env_id` / `env_name` / `job_id` / `agent_model` | `Utf8`（字典） nullable | |
| `blob_manifest_json` | `lance.json` nullable | |
| `unknown_json` | `lance.json` nullable | |

消除二次型冗余的机制在这里：`preferences` 不存 `messages[]`，只存 `step_ordinal`；状态
按需从 `blocks` 取 `step_ordinal <= N` 的前缀重建。

---

## 关联关系与基数

| 父 | 子 | 外键 | 基数 | 语义 |
|---|---|---|---|---|
| `corpus_files` | `file_paths` | `file_digest` | 1 : 1..N | 一份内容可有多个观测路径 |
| `corpus_files` | `attempts` | `file_digest` | 1 : 1..k | 一文件含 `attempts_tried` 个 attempt |
| `attempts` | `analysis` | `attempt_uid` | 1 : 0..21 | analyzer 集合可增长 |
| `attempts` | `steps` | `attempt_uid` | 1 : 0..N | |
| `attempts` | `artifacts` | `attempt_uid` | 1 : 0..M | |
| `attempts` | `preferences` | `attempt_uid` | 1 : 0..N | 仅 `rl_rows` 族非空 |
| `steps` | `steps` | `parent_step_ordinal` | 自引用 | `event_stream` 族的分支树 |
| `steps` | `blocks` | `(attempt_uid, step_ordinal)` | 1 : 0..N | |
| `blocks` | `blocks` | `tool_call_id` | 1 : 0..1 | `tool_call` ↔ `tool_result` 配对 |
| `blocks` | `content` | `content_digest` | N : 1 | **多对一是去重发生的地方** |
| `steps` | `content` | `system_prompt_digest` / `tools_digest` | N : 1 | |
| `analysis` | `content` | `details_digest` | N : 1 | |
| `artifacts` | `content` | `digest` | N : 1 | |
| `preferences` | `content` | `chosen` / `rejected` / `response_digest` | N : 1 | |
| `content` | `tool_defs` | `tools_digest` | 1 : N | 投影展开 |

所有指向 `content` 的边都是 **N : 1**，这是整个布局压缩比的来源。除此之外的边全部是
严格的树形父子关系，没有多对多。

### 关键 join 路径

```text
triage        attempts ⋈ analysis                          （不触碰 content）
成本分析      attempts ⋈ steps                             （不触碰 content）
工具行为      blocks ⋈ steps ⋈ attempts                    （不触碰 content payload）
回放          attempts ⋈ steps ⋈ blocks ⋈ content          （范围读 + blob take）
训练导出      preferences ⋈ blocks ⋈ content
取证          attempts ⋈ artifacts ⋈ content               （显式 opt-in）
```

---

## 身份与幂等

- `content.digest` = BLAKE3-256(未压缩原始字节)
- `corpus_files.file_digest` = BLAKE3-256(文件原始字节)
- `attempts.attempt_uid` = BLAKE3-256(`file_digest` ‖ `attempt_no` 的 LE u32)

**身份取内容而非路径**，带来三个性质：重复入库天然幂等；字节相同的文件自动合并为一个
attempt 并记录多个路径观测；血缘可从任何一层反查到原始文件。

代价是必须接受一个语义判断：字节完全相同（含全部时间戳）的两个文件是**同一次逻辑运行的
拷贝**，而非两次巧合相同的运行。实测的 `_2054` / `_clone_2081` 属于前者。如果某个 harness
确实会产出字节相同的不同运行，则需要在 `attempt_uid` 中混入路径，这是本 RFC 的一个
未决点。

---

## 来源族到 `blocks` 的映射

### `actf_steps`

| 来源字段 | 落点 |
|---|---|
| `steps[i].step_id` | `steps.source_step_id` |
| `steps[i].system_prompt` | `steps.system_prompt_digest` → `content` |
| `steps[i].tools` | `steps.tools_digest` → `content` / `tool_defs` |
| `steps[i].user_content` | `blocks(role=user, block_type=text)` |
| `steps[i].assistant_content.content` | `blocks(role=assistant, block_type=text)` |
| `steps[i].assistant_content.reasoning_content` | `blocks(role=assistant, block_type=thinking)` |
| `steps[i].assistant_content.tool_calls[j]` | `blocks(role=assistant, block_type=tool_call, tool_name, arguments_json)` |
| `steps[i].observation` | `blocks` 行，`role` 取 `tool` 或 `runtime`，`block_type=tool_result`；四种形状统一到 `{content, is_error, tool_call_id}`，未能映射的键进 `unknown_json` |
| `steps[i].metric.*` | `steps` 的摊平列 |

`observation` 的四种形状必须由**形状探测**分派，不得依赖 `declared_schema_version`。

### `event_stream`

| 来源字段 | 落点 |
|---|---|
| 事件 `id` / `parentId` | `steps.source_step_id` / `steps.parent_step_ordinal` |
| `session` 事件的 `cwd` / `version` | `attempts.meta_json` |
| `model_change` / `thinking_level_change` | 生效区间内 `steps.model` / `steps.provider`；原事件保留于 `unknown_json` |
| `message.role=user` 的 `content[]` | `blocks(role=user)` |
| `message.role=assistant` 的 `content[]` 块 | `blocks` 行，`block_type` 取 `thinking`、`text` 或 `tool_call` |
| `message.role=toolResult` | `blocks(role=tool, block_type=tool_result, tool_call_id, tool_name, is_error)` |
| `message.usage.*` / `usage.cost.*` | `steps` 的摊平列 |
| `compaction` 事件 | `steps` 一行，`block_type` 无内容块；标记于 `unknown_json` |
| `custom` 事件 | `steps.unknown_json` |

一次 assistant 消息及其后继 toolResult 折叠为一个 `step`；`parentId` 分叉时
`parent_step_ordinal` 指向分叉点，不做线性化。

### `rl_rows`

| 来源字段 | 落点 |
|---|---|
| `session_id` / `step_id` | `steps.session_id` / `steps.step_ordinal` |
| `messages[]` | **仅最后一条新增消息**落 `blocks`；前缀通过 `step_ordinal <= N` 重建 |
| `chosen_response` / `rejected_response` / `response` | `preferences.*_digest` |
| `reward` / `step_reward` / `is_*` | `preferences` |
| `blob_manifest` / `meta_json` | `preferences` 的 `lance.json` 列 |

入库时必须校验"第 N 步的 messages 是第 N+1 步的前缀"这一前提；若不成立则退化为全量落
`blocks`（CAS 仍会去重相同消息），并在 `corpus_files.shape_fingerprint` 上标注。

---

## Lance 物理决策

1. **`lance.json` 用于小而需查询的半结构化列**：`blocks.arguments_json`、
   `attempts.meta_json` / `ground_truth_json`、`analysis.details_inline_json`、
   `preferences.blob_manifest_json`，以及每张表的 `unknown_json`。这些列在
   `lance 9.0.1` 中以 JSONB（`LargeBinary` + `lance.json` 扩展）存储，可用
   `json_get_*` / `json_extract` 过滤，并可对热路径建 JSON 标量索引。
   相比 `Utf8` 兜底列，它的类型不撒谎且可查询。
2. **JSON 标量索引**建在实际热路径上：`blocks.arguments_json` 的 `$.command`、`$.path`。
   索引按路径字面量匹配，查询必须用同一路径写法。
3. **Blob v2 仅用于 `content.payload_blob`**，且 `content` 是唯一使用 blob 的表。
   不投影即零字节读取。
4. **排序与索引**：各表按第一列为 `attempt_uid` 的复合序物理排序；`attempt_uid`、
   `content.digest`、`blocks.tool_name` 上建标量索引。
5. **不使用 Variant**：Lance 明确不实现 Parquet Variant；`parquet-variant` 仍标注 WIP
   且属 Parquet 侧。JSONB 是当前唯一可用的原生二进制 JSON 路径。
6. **兜底列必须存在于每张表**。198 个键名、四种同版本异构形状、按来源出现的可选字段，
   决定了任何封闭 schema 都会丢数据。

---

## 查询路径示例

```sql
-- triage：只碰 L1，不读一个内容字节
SELECT a.task_id, a.benchmark, a.model_tag, a.score, an.analyzer
FROM attempts a JOIN analysis an USING (attempt_uid)
WHERE a.correct = false AND an.is_badcase
  AND a.benchmark = 'skillsbench';

-- 成本分析：只碰 L1 + L2 的窄列
SELECT model_tag, sum(output_tokens), avg(llm_infer_ms)
FROM attempts a JOIN steps s USING (attempt_uid)
GROUP BY model_tag;

-- 工具行为：碰 L2，payload 不物化
SELECT tool_name, count(*), sum(CAST(is_error AS INT))
FROM blocks
WHERE block_type = 'tool_call'
GROUP BY tool_name;

-- 命令级下钻：走 JSON 索引
SELECT attempt_uid, step_ordinal
FROM blocks
WHERE tool_name = 'Bash'
  AND json_get_string(arguments_json, 'command') LIKE '%pyomo%';
```

前三个查询完全不接触 L3 / L4。这正是分层的目的。

---

## 生命周期与 GC

- **L4 可过期**：`artifacts` 中 `derives_from = 'trajectory'` 的行可按保留期删除，
  仅需级联清理 `content` 中失去引用的 digest。删除后 L1/L2/L3 完整，全部分析查询不受影响。
- **`content` GC** 是引用计数：live set = `blocks` ∪ `steps` ∪ `analysis` ∪ `artifacts`
  ∪ `preferences` ∪ `corpus_files` 中出现的全部 digest。因为 `content` 是所有层共享的，
  GC 必须在**所有引用表的同一快照**上计算，否则会误删。
- **重建**：L1/L2/L3 全部可从 `corpus_files.raw_digest` 重放重建。若不保留原始 JSON，
  则 L2/L3 成为事实源，此时 L4 的删除不可逆——这是一个需要显式配置的取舍。

---

## 凭证脱敏

实测在 `attempts.*.meta.plan` 下发现明文凭证：
`environment.params.secret_key`（10 处）与
`execution.analysis_params.OnomyAnalyzer.api_key`（2 处）。

因此入库管线 **MUST** 在写入前对 `meta_json` 执行键名模式脱敏
（`secret` / `token` / `api_key` / `password` / `credential` / `authorization`），
将值替换为 `«redacted:<blake3-8>»` 形式的占位符，保留可比较性而不保留明文。
`corpus_files.raw_digest` 指向的原始 JSON 若保留，则该 dataset 必须与 L1–L4 分离
授权。这是本 RFC 的一条硬约束，而不是建议。

---

## 预估收益

按实测推算：

| 项 | 原始 | 目标 |
|---|---|---|
| L4 取证层 | ~670MB | ~110MB（CAS + zstd），且离开查询路径 |
| L0–L3 | ~220MB | ~15–20MB（去重 2.2x × 压缩 ~6x） |
| **合计** | **1562MB** | **~20MB 可查询 + ~110MB 冷 blob** |
| triage 查询实际读取 | — | 几百 KB 量级 |

若判定 L4 可丢（它确实是 L2+L3 的衍生重复品），则总量约 20MB。

---

## 未决问题

1. **字节相同文件的身份**：`attempt_uid` 是否应混入路径？取决于是否存在会产出字节相同
   的不同逻辑运行的 harness。当前假设不存在。
2. **step 折叠粒度**：`event_stream` 族一次 assistant + 后继 toolResult 折叠为一 step 是
   启发式；`compaction` 事件与多 toolResult 的情形需要更明确的规则。
3. **与 Storyline 的关系**：本布局是否应作为 Storyline 三表的上游语料层，还是应由
   Storyline 增补 attempt / analyzer / artifact 维度后统一？前者引入两套模型，后者会把
   评测语料的关注点压进互操作枢纽。
4. **`observation` 四形状的收敛**：统一到 `{content, is_error, tool_call_id}` 会丢失
   形状 3 的 `tool_names`（复数）语义，目前靠 `unknown_json` 兜底，是否需要一等列待定。
5. **inline 阈值**：8KB 为估计值，需按实际块大小分布（实测消息平均 665 字节）实测标定。
