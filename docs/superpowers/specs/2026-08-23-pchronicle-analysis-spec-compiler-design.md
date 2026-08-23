# AnalysisSpec 编译器设计

日期：2026-08-23
状态：待审
范围：pChronicle Analyze 工作区（`pchronicle-web` Analysis 模块）与 `pchronicle serve` 的 query catalog / evidence 路径
取代：`AnalysisPlan.sql` 作为可执行契约的设计（见 `2026-08-22-pchronicle-copilot-analysis-workspace-design.md`）。本规格不废除 plan-review-run 状态机，而是把「模型写 SQL、用户确认再跑」改成「模型写 Spec、编译器出 SQL」。

## 1. 决策

**SQL 不是 Analysis 的功能入口，而是内部编译产物。**

用户面对的是可计算的分析问题。系统把它编成一份用户可读的 `AnalysisSpec`，再由确定性编译器根据真实 schema 生成只读 SQL。模型不写 SQL。后端不判断「任务是否完成」或「为什么失败」。

锁定方案 **A**：

- LLM 只产出或修订 `AnalysisSpec`。
- 服务端编译器根据 live schema 生成 SQL。
- 编译 / EXPLAIN / 结果形状失败时，把结构化错误回给模型改 Spec，最多两次。不重放同一条错误 SQL。
- 高级详情中的 SQL 是只读编译结果。
- 用户手写 SQL 是逃逸舱：跳过编译器和 Spec 修复循环，仍走 EXPLAIN 与有界执行。

## 2. 要解决的硬问题

这些已经在 2026-08-23 的源码与实机中坐实，不是推测：

1. **构建产物漂移。** 当前源码是 Analyze 工作区；若运行中的 `pchronicle serve` 仍嵌入旧 SQL Workspace，用户看到的不是正在维护的界面。验收必须包含：serve 二进制与 `pchronicle-web` 同源构建。
2. **理想 mock 能走通，真实 schema 不能。** 现链路「问题 → AnalysisPlan → SQL → 结果 → 解释」在 mock 下成立，只能证明状态机，不能证明契约。
3. **Catalog 说谎。** `/api/query/tables` 由 `run_query_fields()` 等硬编码表提供。它宣称 `runs.run_id_explicit`；Arrow 引擎字段是 `trajectory_id_explicit`（`story_runs_arrow_schema()`）。模型越遵守 Catalog，SQL 越容易失败。
4. **Retry 不修正。** `QueryError` 文案为 “The SQL is unchanged. Retry when ready.”；按钮只是再执行同一条 SQL。DataFusion 错误原文进入界面。

当前推荐问题也超出数据模型：

| 推荐问题 | 模型会假设的列 | 查询引擎实际 |
|---|---|---|
| 比较成功和失败的 run | `runs.status` | `runs` 表无统一 status。Explorer 的 `RunSummary.status` 不是 SQL 列。 |
| 找 latency 离群 | run 级 latency | 一等列是 `steps.latency_ms`、`tool_calls.duration_ms`。`runs.final_metrics_json` 的 latency 口径未定义。 |
| 按工具统计显式错误 | tool error 字段 | `tool_calls` 无规范化 error/status 列。 |
| 为什么失败 | 因果事实 | SQL 只能取证。解释属于 AI，且必须标成 inference。 |

## 3. 产品契约

### 3.1 v1 只回答五类可计算问题

`intent` 必须是下列之一。其它问题编译失败，界面说明需要先下钻取证，而不是生成 SQL。

| Intent | 用户问题形态 | SQL 做什么 | AI 解释做什么 |
|---|---|---|---|
| `distribution` | 延迟、步数、工具调用次数如何分布 | 聚合或分桶 | 描述分布，不编造成因 |
| `compare` | 两个 cohort / model / agent / version 的指标差 | 分组对比 | 描述差值，不宣称哪个「更好」除非差值在表里 |
| `rank_outlier` | 最慢、次数最多、异常值候选 | ORDER BY + LIMIT 或分位数过滤 | 指出候选，不把离群说成故障 |
| `composition` | 工具、模型、来源与指标的分组关系 | GROUP BY | 描述构成 |
| `drilldown` | 找出对应 run / step / tool call 当证据 | 筛选行，保留 identity 列 | 把行当成证据入口，不直接回答「为什么」 |

「为什么失败」「任务是否真正完成」「语义是否正确」不是 intent。它们只能出现在解释阶段，且必须放进 inferences / limitations，不能当作 SQL 目标。

### 3.2 推荐问题

Analyze 页的 starter 只能覆盖上表，并且只能引用第 6 节登记过的 measure / dimension。v1 删除这三条 starter：

- Compare successful and failed runs in this scope
- Find latency outliers and the tools associated with them（若按 run 级 latency 理解）
- Summarize explicit errors by tool and model

替换为可编译的例子，例如：

- 按 `agent_model_name` 对比每个 run 的 step 数
- `steps.latency_ms` 的分布，以及最慢的 20 个 step
- 按 `function_name` 统计 tool call 次数，并下钻到调用最多的 run

### 3.3 页面对象是 AnalysisSpec，不是 SQL Plan

现有 `AnalysisPlan` 的 `filters` / `groupings` / `measures` / `expected_columns` / `suggested_view` 只是 LLM 旁白，真正执行的是 `sql` 字符串。本规格 **替换** `AnalysisPlan` 为 `AnalysisSpec` + 编译产物，禁止两套并列。

## 4. AnalysisSpec

```text
AnalysisSpec
  intent          五类之一
  scope           dataset / root / run（沿用现有 AnalysisScope）
  grain           run | step | tool_call
  measure         已登记口径，见第 6 节
  dimension       可选；compare / composition 必填
  filters         已登记字段上的谓词，不含自由 SQL
  ranking         可选 Top / Bottom / Outlier
  assumptions     编译器将写入实际用到的口径，模型不得发明
  output          table | distribution | comparison
  identity_columns 下钻时必须包含的 identity，由编译器填
```

规则：

- 模型可以填 `intent`、`grain`、`measure`、`dimension`、`filters`、`ranking`、`output`。
- 模型不得填写不存在的列名。编译器对照 live schema 校验；未知列 → 编译失败。
- `assumptions` 与 `identity_columns` 由编译器写入。若模型自带，编译器覆盖。
- `uncomputable_reason` 只由编译器填写。例如请求 run 级 status 时返回明确原因，而不是猜 `final_metrics_json`。
- Spec 必须能在无 SQL 的情况下让人读懂：分析哪批数据、按什么粒度、算什么、按什么比、口径是什么。

Filter 谓词只允许：`eq`、`neq`、`in`、`not_null`、`is_null`、`gt`、`gte`、`lt`、`lte`、`like`。操作数是字面量。禁止嵌入 SQL 片段。

Ranking：

- `top_n` / `bottom_n`：按 measure 排序，n 默认 20，上限 100。
- `outlier`：v1 只支持「measure 高于同 scope 内 P95」。没有 P95 列可用时编译失败，不改用 JSON。

## 5. 职责切分

| 组件 | 拥有 | 不拥有 |
|---|---|---|
| 前端 Analyze UI | 问题、单按钮编排、展示 Spec、折叠只读 SQL、表格/分布、基于证据的解释 | 生成 SQL、发明列名、判断任务成败 |
| 前端 LLM | 从问题生成 / 修订 Spec | 写 SQL、调用 EXPLAIN、执行查询 |
| `pchronicle serve` | live schema、`compile(spec)`、EXPLAIN、有界执行、结果形状校验 | 语义判断、把 JSON 当一等度量、自动「修 SQL」 |
| 查询引擎 | DataFusion 只读 SELECT / WITH / EXPLAIN |  |

编译器必须与引擎同进程（CLI server），禁止在 WASM 里根据过期 Catalog 拼 SQL。

## 6. 已登记口径（v1）

编译器只映射下表。表中没有的 measure / dimension 一律编译失败，不得用 `json_extract` / `final_metrics_json` / `metrics_json` / `extra_json` 补。

### 6.1 Grain → 表

多 dataset 时表名为 `{dataset}.runs` 等，与当前 `queryable_tables()` 的限定名一致。

| Grain | FROM | Identity 列（下钻必带） |
|---|---|---|
| `run` | `{dataset}.runs` | `_file_`、`session_id`、`document_id`、`agent_id` |
| `step` | `{dataset}.steps` | `_file_`、`session_id`、`step_id`、`document_id` |
| `tool_call` | `{dataset}.tool_calls` | `_file_`、`session_id`、`step_id`、`tool_call_id`、`function_name` |

Scope 编译为 WHERE：

- Dataset → 只使用该 dataset 的限定表名，不加 `_file_ LIKE`。
- Root → 编译器在服务端用现有 explorer/run 索引把 `root_session_id` 展开为 `session_id` 列表，生成 `session_id IN (...)`。禁止对 `parent_json` 做推断。展开结果为空则编译失败。
- Run → `_file_` 与 `session_id` 等值过滤；有 `document_id` 时一并等值。

### 6.2 Measures

| 登记名 | 可用 grain | SQL 含义 | 口径 |
|---|---|---|---|
| `row_count` | 任一 | `COUNT(*)` | 该 grain 的行数 |
| `step_count_per_run` | `run` | 对 steps 按 run identity 计数后挂回 | 每个 run 的 step 行数 |
| `tool_call_count_per_run` | `run` | 对 tool_calls 按 run identity 计数 | 每个 run 的 tool call 行数 |
| `tool_call_count` | `tool_call` | `COUNT(*)` | 调用次数 |
| `step_latency_ms` | `step` | `steps.latency_ms` | 可空；聚合时忽略 NULL，并在 assumptions 写覆盖率 |
| `step_ttft_ms` | `step` | `steps.ttft_ms` | 同上 |
| `tool_duration_ms` | `tool_call` | `tool_calls.duration_ms` | 同上 |

v1 **不登记**：

- run 级 latency / duration / tokens
- run / step / tool 的 success、failure、error、status
- `final_metrics_json`、`metrics_json` 内的任意键（含 `prompt_tokens`、`prompt_tokens_len`、`total_tokens`）
- Explorer `RunSummary.status`

Token 分布要等一等列存在后再登记。在此之前，starter 和编译器都不得接受 token measure。

### 6.3 Dimensions

只能是 live schema 中的一等列，且类型为 utf8 / bool。v1 白名单：

- runs：`agent_id`、`agent_name`、`agent_version`、`agent_model_name`、`_file_`
- steps：`source`、`effective_kind`、`model_name`、`had_tool_calls`
- tool_calls：`function_name`

跨 grain 取维（例如 tool call 按 `agent_model_name` 分组）必须由编译器生成对 `runs` 的 JOIN，JOIN 键为 `_file_` + `session_id`。模型不得自己写 JOIN。

### 6.4 Output 形状

编译器声明 expected schema，执行后校验：

| output | 必须出现的列 | 视图 |
|---|---|---|
| `table` | identity 列 + measure 列 | Result Explorer 表 |
| `distribution` | measure 列；可选 bucket 列 | 表 + 分布图 |
| `comparison` | dimension 列 + measure 列 | 表 + 分组条 |

行数仍受现有 interactive bound（100 行 / 4MB）约束。校验失败 → `ShapeError`，进入 Spec 修复，不把残缺表送给解释模型装成完整证据。

## 7. Live schema

`GET /api/query/tables` 必须从 DataFusion 已注册表内省字段，而不是 `run_query_fields()` 这类静态表。

规则：

- 字段 **名** 与 **空值性** 来自引擎 schema（含 catalog 注入的 `_file_`）。
- 表 **名** 必须是 DataFusion 已注册的限定名（如 `test.runs`），与 evidence SQL 里可引用的名字一致。禁止 Catalog 返回 `runs`、前端再猜测前缀。
- 字段 **描述** 可以是按字段名查表的文案；查不到就空字符串。描述不得创造引擎里没有的字段。
- 回归测试：对每个 dataset 表，Catalog 返回的 `fields[].name` 集合等于 `SHOW COLUMNS` / provider schema 的字段名集合。今日的 `run_id_explicit` 必须消失；`trajectory_id_explicit` 必须出现（若引擎有该列）。
- `sources.status` 仍表示 source 投影状态（ready / error），不得被编译器当成 run 成败。

## 8. 编译与执行链路

```text
用户问题
  → LLM：生成 AnalysisSpec
  → POST /api/analysis/compile { spec, snapshot_id }
       校验 intent / grain / measure / dimension / filters
       对照 live schema
       生成 SQL
       EXPLAIN
       返回 { spec, sql, explain_ok } 或 CompileError
  → 失败且 repair_count < 2：把 CompileError 回给 LLM 修订 Spec
  → POST /api/query/evidence { sql }   （现有有界执行）
  → 校验结果形状
  → 表格与分布
  → LLM：只读证据解释（Observed / Inference / Limitation）
```

`snapshot_id` 必须与当前 Catalog 一致。不一致 → 编译拒绝，提示刷新 Catalog。不静默对新快照执行旧 Spec。

EXPLAIN 已由 `validate_read_only_sql` 允许。编译器用 `EXPLAIN SELECT ...` 预检；EXPLAIN 失败与未知列一样走 Spec 修复。

自动修复的对象是 Spec，不是 SQL。第三次仍失败则停在可操作错误，按钮回到 Analyze，不允许「Retry analysis」重放 SQL。

### 8.1 API

新增 `POST /api/analysis/compile`：

请求：`{ spec, snapshot_id }`
成功：`{ spec, sql, assumptions, identity_columns, expected_columns, output }`
失败：`{ code, message, field, engine_detail? }`

`message` 给用户和 LLM，短、可操作（例如 “runs 没有 status；v1 不能按成功/失败分组”）。`engine_detail` 默认折叠，上限 1500 字符，沿用 `query_evidence_error` 的截断。界面主文案不得展示完整 DataFusion 栈。

手写 SQL 逃逸舱继续使用 `POST /api/query/evidence`。该路径：

- 不做 Spec 修复。
- 仍只读、仍有界。
- 失败时同样只展示截断摘要。
- 不自动调用解释模型，除非用户在逃逸舱明确点解释（v1 可以不做解释，避免把随意 SQL 当成证据契约）。

### 8.2 编译器实现约束

- 输出单一 `SELECT` 或 `WITH ... SELECT`。
- 禁止 `;`、写语句、文件扫描之外的函数。
- 外层 LIMIT 仍由现有 `bounded_evidence_sql` 包一层。
- 确定性：同一 spec + 同一 schema → 同一 SQL（空白规范化后）。用单元测试钉死。
- 编译器是纯函数：`compile(spec, schema, scope) -> Result<CompiledQuery, CompileError>`，放在 `persisting-pchronicle` 或 CLI server 可测模块，不放进 WASM。

## 9. 前端

### 9.1 单按钮

主按钮只有 **Analyze**。取消并列的 Generate plan / Run analysis / Retry analysis。

状态文案：

| 内部状态 | 界面 |
|---|---|
| 生成 Spec | 理解问题 |
| 编译 + EXPLAIN | 验证查询 |
| 把错误送回 LLM 改 Spec | 修复规格 |
| 有界执行 | 执行 |
| 解释 | 解释 |

未配置模型时，Analyze 打开模型设置，不生成半份 SQL。

### 9.2 Spec 与 SQL 的展示

默认展示可读 Spec（scope、grain、measure、dimension、filters、assumptions、output）。SQL 在「高级详情」中折叠，只读。点字段插入 SQL 的 schema 浏览器只在用户打开逃逸舱（手写 SQL）时出现。

### 9.3 与现有会话存储

`AnalysisRevision.plan: Option<AnalysisPlan>` 改为 `spec: Option<AnalysisSpec>` 加 `compiled_sql: Option<String>`。旧 localStorage 里只有 `plan.sql` 的 session：打开时标为 stale，要求重新 Analyze，不尝试执行旧 SQL。

解释阶段继续使用现有 Observed / Inference / Limitation 结构。解释 prompt 吃 evidence digest + Spec，不吃「请根据 SQL 推断失败原因」。Digest 截断时 limitations 必须写明。

### 9.4 Copilot

本规格不合并 Copilot 与 Analyze。Copilot 仍是轨迹 overlay。Analyze 是仓库级取证。两者文案需要一句分工，但不在本规格实现 Copilot。

## 10. 非目标

- 不在 v1 把 Trace 改成时间旅行调试器。
- 不投影新的 token / status / tool error 列。需要时另开规格。
- 不让后端 LLM 化。模型调用仍在前端。
- 不做多语句脚本、物化、写回。
- 不把 Compare 做成独立 trajectory compare 工作区（已有另一份 compare spec）。
- 不修复 9966/9967 上的旧进程本身；验收是「当前构建的 serve 嵌入当前 web」。

## 11. 测试

必须有、且不依赖真实 LLM：

1. **Schema 一致性。** Catalog 字段名 == 引擎 schema 字段名。包含 `trajectory_id_explicit` 在、`run_id_explicit` 不在的断言。
2. **编译快照。** 每个 intent 至少一条 spec → SQL 金丝雀。
3. **拒绝。** status / token / `final_metrics_json` / 未知列 / 五类外 intent 都返回 CompileError，且不产生 SQL。
4. **修复循环。** 模拟第一次 CompileError、第二次成功；第三次失败停止。断言从未把错误 SQL POST 到 evidence。
5. **形状校验。** 缺 identity 列的 drilldown 结果不能进入解释。
6. **前端状态机。** 单按钮 Analyze 覆盖 Draft → 完成；QueryError 不再出现「SQL unchanged / Retry analysis」。

LLM 相关测试继续用 fixture 返回合法 / 非法 Spec JSON，不在 CI 打真实模型。

## 12. 实施顺序

同一份规格，落地顺序不可对调：

1. Live schema（`/api/query/tables` 内省）。没有地面，编译器会把谎言编译成 SQL。
2. `AnalysisSpec` 类型、口径表、`compile()` 纯函数与拒绝测试。
3. `POST /api/analysis/compile` + EXPLAIN。
4. 前端替换 Plan：单按钮、状态、折叠 SQL、新 starter、旧 session stale。
5. 解释层改为基于 Spec + digest；错误摘要折叠 engine_detail。
6. serve 嵌入构建：`just chronicle-web-build`（或现有等价步骤）后的 CLI 才算完成。

## 13. 验收

- Catalog 字段名与引擎一致；幽灵列（`run_id_explicit`）为零。
- 主路径零 SQL 编辑；用户不需要先 Generate plan 再 Run。
- Retry 不再重放错误 SQL；修复只改 Spec。
- 用户主路径看到的是可操作失败摘要，不是整段 DataFusion 栈。
- 推荐问题均可被编译器接受，或在编译期被拒绝并说明不可计算；不得生成「看起来能跑、对错列」的 SQL。
- 「为什么失败」不能作为 intent；只能作为解释阶段的 inference。
- 后端不输出任务完成与否的判断。
- 当前源码构建出的 `pchronicle serve` 打开的就是本 Analyze 工作区，而不是旧 SQL Workspace。
