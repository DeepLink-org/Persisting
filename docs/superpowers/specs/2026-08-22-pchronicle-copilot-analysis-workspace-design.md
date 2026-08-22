# pChronicle Copilot 分析工作台设计

**日期：** 2026-08-22

**状态：** Awaiting written review（方向已在对话中确认）

**范围：** `pchronicle-web` 桌面端 Analyze 页面。复用现有只读查询 API 和浏览器
BYOK 配置；不引入服务端 Copilot、服务端分析判断或 Search 子系统。

## 背景

现有 Analyze 页面是一个面向数据库用户的只读 SQL 控制台：左侧展示 virtual
tables 和字段，右侧提供 path filter、SQL textarea 与结果表。它能执行自由查询，
但要求用户理解 pChronicle 的虚拟表结构并亲自编写 SQL。结果只有表格，缺少列画像、
分布探索、轨迹下钻和基于证据的结论。

目标用户通常已经有一个需要验证的问题，例如：

- 同一任务下成功与失败运行的工具使用有何差异；
- 哪类工具调用与较高延迟或显式错误同时出现；
- 某个 root 下不同运行的 token、latency 或错误分布是否明显不同。

用户的心智是“描述问题并验证证据”，不是“打开数据库 IDE”。因此 Analyze 应从
SQL-first 控制台改为问题驱动的分析工作台。Copilot 负责把问题转换成可审阅的只读
SQL；用户负责确认执行、探索结果并决定下一步。SQL 保留为透明、可编辑但弱化的
中间产物。

结果探索参考 Hugging Face Data Studio 的列分布体验：表格仍是证据主体，列头直接
展示基本分布，选择列后展开详细画像。pChronicle 另外必须明确任意 SQL 结果的返回
行预算，不能把截断预览的分布描述成全量事实。

## 产品定位

Analyze 是一个“问题驱动的轨迹证据分析工作台”，完成如下闭环：

```text
描述问题与范围
  → Copilot 生成分析计划和 SQL
  → 用户审阅并确认执行
  → 浏览表格、列分布和轨迹证据
  → Copilot 区分事实与推断地解读结果
  → 用户选择后续问题或可视化区间
  → Copilot 生成下一份待确认方案
```

SQL 永远不能因为模型输出或可视化点击而自动执行。每次查询都经过用户可见的确认
步骤；服务端继续负责最终的只读 SQL 校验与结果预算。

## 目标

1. 用户使用自然语言描述已有问题，无需知道 virtual table 和字段名。
2. Copilot 结合当前 catalog 与显式分析范围生成结构化计划及只读 SQL。
3. 用户能在执行前看懂范围、分组、指标和过滤条件，并一键确认。
4. SQL 默认折叠；高级用户可展开并编辑。
5. 结果表内嵌列分布、缺失率和类型摘要，并可展开列画像。
6. 明确区分“返回行预览统计”和“全量结果统计”。
7. 查询结果中的 run、turn、tool call 等身份可直接下钻到轨迹证据。
8. Copilot 自动给出事实、推断、限制与后续问题，且结论可回到结果证据验证。
9. 后续问题、列分桶和 full profile 都生成新的待确认计划，不静默执行。
10. 分析会话在本地浏览器恢复；pChronicle 服务端不保存会话或模型密钥。

## 非目标

- 移动端或窄屏适配。
- 通用数据库 IDE、完整 SQL Notebook 或 BI Dashboard。
- SQL 自动补全、数据库对象管理、写查询或多数据库连接。
- 模型自动执行 SQL 或连续自主查询。
- 服务端 Copilot session、服务端代持 API key、服务端生成结论。
- 保存完整查询结果集或把大体积结果写入 `localStorage`。
- 全量数据统计的隐式后台预计算。
- Search、Queue/Samplers、TTAS、tiered memory 或 `persisting-dlcapt`。
- 第一阶段实现跨用户协作、云端分享、报告发布或调度分析任务。

## 核心用户流程

### 新建分析

1. 用户进入 Analyze。
2. 页面显示当前 dataset、storage path、只读状态和可选范围标签。
3. 用户输入自然语言问题，或选择一个问题模板后修改。
4. Copilot 只生成计划，不调用查询端点。
5. 页面进入 `Plan ready`，展示意图、范围、过滤、分组、指标和折叠 SQL。
6. 用户点击 `Run analysis` 后才调用现有只读查询 API。

### 高级 SQL

- `Advanced · view or edit SQL` 默认折叠，视觉优先级低于计划摘要。
- 展开后显示完整 SQL；编辑不会立即执行。
- SQL 被编辑后计划标记为 `Manually edited`，记录生成 SQL 与当前 SQL 已分叉。
- 用户仍通过同一个 `Run analysis` 按钮确认执行。
- 用户可要求 Copilot 根据自然语言重新生成；重新生成覆盖当前草稿前需明确提示。

### 探索结果

1. 查询成功后，结果区显示匹配/返回行数、预算、截断与执行信息。
2. 表头展示每列的轻量画像；表格展示原始证据行。
3. 点击列头后展开详细 column profile。
4. 点击分类值或直方图区间只形成一个可见的 refinement intent。
5. 用户要求应用后，Copilot 基于原查询和 refinement intent 生成新计划。
6. 新 SQL 再次等待确认，不在浏览器中直接拼接并执行。

### 解读与追问

1. 查询结果可用后，前端生成有界 evidence digest 并请求 Copilot 解读。
2. 表格立即可用；解读异步完成，不阻塞结果探索。
3. Copilot 返回观察事实、可能解释、限制和后续问题。
4. 用户点击后续问题时，创建下一次 analysis revision，并返回 `Plan ready`。
5. 历史 revision 折叠成时间线节点；当前 revision 始终是视觉主体。

## 分析范围

范围必须在页面上显式可见，模型不得暗中扩大或缩小范围。

### 默认范围

- 从 Analyze 一级导航进入：当前 dataset/catalog。
- 从 Run Detail 进入：当前完整 run coordinates。
- 从某个 root 上下文进入：该 root。
- 从 Pinboard 进入：用户选择的 pinned runs。
- 从 Compare 进入：左右两条 run coordinates。

范围使用可移除 chips 展示。移除或增加范围会使当前未执行计划标记为 stale，并要求
重新生成。已执行结果保留原始 scope snapshot，不随页面当前 chips 改写。

### 传给模型的上下文

生成计划时只发送：

- 用户问题；
- catalog 中的 table/field 名称、类型、grain 和描述；
- 显式 scope coordinates；
- 上一 revision 的计划摘要与有限结果摘要（追问时）；
- read-only、结果预算和禁止自动执行的系统约束。

不把整个轨迹、完整目录或未请求的 result rows 自动发送给模型。

## 页面信息架构

### 页头

- `pChronicle / Analyze / <database>` breadcrumb；
- 当前 storage/dataset；
- catalog 状态；
- `Read-only` 状态；
- 清除本地分析记录和模型 Settings 的低优先级入口。

### 问题区

- 自然语言 textarea 是首要输入；
- 显式 scope chips 位于输入附近；
- 主动作是 `Generate plan`；
- 空状态提供少量轨迹分析问题模板，不提供 SQL 模板作为主入口。

### 计划确认卡

结构化展示：

- Copilot 对问题的理解；
- scope；
- filters；
- groupings；
- measures；
- 预期结果形态；
- 折叠的高级 SQL；
- `Regenerate` 和唯一高优先级 `Run analysis`。

计划卡不展示模型的自由形式长篇推理。

### 结果区

从上到下依次为：

1. 执行摘要和预算/截断状态；
2. Copilot 结果解读；
3. 可视化筛选 chips；
4. 表头内嵌列画像的结果表；
5. 选择列后的详细 profile；
6. 可点击的后续问题。

桌面宽度足够时，详细 profile 位于表格右侧；空间不足时置于表格下方。产品范围不
要求移动端重新组织导航或触控交互。

### 会话历史

Analyze 使用 analysis session，而不是普通聊天气泡流：

- 当前 revision 展开；
- 旧 revision 以问题、状态、时间、行数的紧凑节点折叠；
- 点击旧节点恢复当时的计划、执行摘要和解读；
- 未持久化的完整结果需要用户重新执行后才能恢复。

## 结构化计划契约

Analyze 使用独立于单 Run `Trajectory Copilot` 的工作流。它可复用 LLM transport、
配置和 provider fallback，但不复用当前会自动执行 `query_sql` 的 tool loop。

模型生成结果必须解析为 `AnalysisPlan`：

```text
AnalysisPlan
  id
  question
  intent_summary
  scope_summary
  filters[]
  groupings[]
  measures[]
  expected_columns[]
  suggested_view
  sql
  warnings[]
```

`suggested_view` 只影响结果区的初始展示建议，不允许模型提供可执行 JavaScript、HTML
或任意图表配置。前端根据返回列的实际类型决定最终列画像。

如果 provider 支持 structured output，则请求 JSON schema；否则使用严格 JSON prompt
并在前端验证。解析失败最多请求模型修复一次；仍失败则显示可重试错误，不执行从
自由文本中猜出的 SQL。

## 状态模型

每个 `AnalysisRevision` 只能处于以下状态之一：

```text
Draft
GeneratingPlan
PlanReady
Executing
Interpreting
Complete
PlanError
QueryError
InterpretationError
Stale
```

关键约束：

- 只有用户明确点击执行或重试，才能从 `PlanReady` 或 `QueryError` 进入
  `Executing`；
- `GeneratingPlan` 完成绝不能直接进入 `Executing`；
- query 返回后先保存 evidence，再进入 `Interpreting`；
- interpretation 失败时结果表仍保留，状态为 `InterpretationError`；
- scope/catalog 变化只把未执行计划变为 `Stale`，不篡改历史结果；
- 重试 query 使用同一份已确认 SQL；重新生成 plan 创建新 plan version。

## 查询执行与数据流

服务端不新增分析判断：

1. 前端通过现有 `/api/query/tables` 加载 catalog。
2. 浏览器直接调用用户配置的 OpenAI-compatible endpoint 生成 `AnalysisPlan`。
3. 用户确认后，前端调用现有 `/api/query/evidence`。
4. 服务端继续执行 SELECT/WITH/EXPLAIN 白名单、行数和字节预算。
5. 前端从 `QueryEvidence.rows` 计算 preview column profiles。
6. 前端构建有界 evidence digest，直接发送给 BYOK 模型生成 interpretation。
7. 后续问题回到步骤 2，不能由 interpretation 调用查询工具。

第一阶段沿用 interactive query 的 100 行、4 MiB 上限。服务端响应中的实际
`returned_rows`、`truncated`、`max_rows` 和 `max_bytes` 必须原样进入结果状态。

## Result Explorer

### 表格是证据主体

- 保留现有 `DataTable` 的横向滚动、结构化 JSON 渲染和单元格展开能力；
- 增加 sticky profile header；
- 数值右对齐，null、boolean、status 使用稳定语义；
- 识别 `_file_`、`run_id`、`session_id`、`root_session_id`、`turn_id` 等身份列；
- 具有足够 coordinates 时渲染为 Run/Turn deep link；坐标不足时仍显示普通值，不猜测。

### Preview column profiles

所有 preview profiles 只基于 `QueryEvidence.rows`，完全在前端确定性计算。

| 推断类型 | 表头摘要 | 展开画像 |
|---|---|---|
| number | min/max、missing、mini histogram | ≤10 bins、min/max/mean/median、distinct |
| boolean | true/false 比例 | counts、missing |
| categorical string | Top values bars、unique、missing | Top 10 + other、counts |
| free text | 字符长度 mini histogram | 长度分布、min/max/mean、missing |
| datetime | 时间 mini histogram | 时间范围与 ≤10 bins |
| array/object | present/missing、shape marker | 常见顶层 key 或数组长度分布 |
| identifier | unique、missing | identity 摘要与 deep-link 说明 |

类型推断必须稳定并可测试：

- 忽略 null 后全部为 JSON number 才是 number；
- 全部为 boolean 才是 boolean；
- 字符串只有在所有非空值都通过同一严格 datetime parser 时才是 datetime；
- 字符串 unique count ≤20 且 unique ratio ≤0.5 时视为 categorical；
- 其他字符串为 free text；
- 已知 identity column 名优先判定为 identifier；
- 混合 scalar 类型标记为 mixed，只显示 type counts 和 missing，不画误导性 histogram。

Histogram 使用最多 10 个等宽 bins。空列、单值列和非有限数值有专门状态，不制造
虚假的范围。Top values 采用稳定排序：count 降序，序列化值升序作为 tie-breaker。

### Preview 与 full profile

结果区始终显示 profile scope：

- 未截断：`Distribution of all returned rows`；
- 截断：`Preview distribution · N returned of a larger result`；
- `returned_rows = 0`：不显示分布，展示空结果状态。

前端不得声称 preview 代表完整 query population。用户点击 `Prepare full distribution
query` 后，创建一个 refinement intent，由 Copilot 生成 `COUNT`、Top-K 或 bucket
aggregate SQL；仍需用户确认执行。所谓 full 仅表示该聚合 SQL 在服务端查询范围内的
结果，不绕过服务端数据或资源边界。

### 可视化筛选

- 点击 categorical bar 或 histogram bin 只暂存人类可读的 refinement intent；
- 页面显示待应用的列、值/区间与原始 query revision；
- `Apply through Copilot` 生成新 plan；
- 前端不直接把值拼接进 SQL，也不本地过滤后伪装成全量查询；
- 用户取消 chip 不改变已有结果。

## Copilot 结果解读

查询完成后，前端构造 `EvidenceDigest`，最大 64 KiB：

- 原问题、scope 和 plan 摘要；
- 执行 SQL hash 与可见 SQL；
- columns 与 preview profile 摘要；
- 有界、稳定选取的 result rows；
- returned/truncated/max budget；
- 可生成 deep link 的 row identity references。

超过预算时先保留 schema、profiles、限制信息，再按稳定顺序裁剪 rows。裁剪必须标记，
不能静默发生。

模型返回结构化 `AnalysisInterpretation`：

```text
observations[]   // 只陈述 digest 可直接支持的事实
inferences[]     // 明确标注为可能解释
limitations[]   // truncation、missing、coverage、sampling
follow_ups[]     // 自然语言问题，不含自动执行动作
references[]    // result row / run / turn identity
```

界面将 `observations` 与 `inferences` 分区显示，不把两者合并成一个“结论”。无法解析
interpretation 时显示原始结果和重试入口，不降级为未经约束的自由文本。

## 本地分析会话

### 存储内容

键按 catalog/storage identity 分区，例如：

```text
pchronicle_analysis:<storage_fingerprint>:<session_id>
```

保存：

- session 标题和更新时间；
- 每个 revision 的问题、scope、计划、SQL、状态；
- 执行摘要、profile 摘要、interpretation；
- result identity references；
- 是否需要重新执行。

不保存：

- 完整 `QueryEvidence.rows`；
- API key 的副本；
- 未裁剪 tool/LLM 原始响应；
- 服务端不可恢复的临时对象。

### 预算与清理

- 最多保留最近 20 个 analysis sessions；
- 每个 session 编码后上限 256 KiB；
- 超限时先删除最旧 revision 的 profiles/interpretation digest，再删除最旧 session；
- `Clear analysis history` 只清理 analysis keys，不影响 BYOK config 和单 Run Copilot；
- localStorage 不可用或 quota 失败不阻断当前分析，只提示本次不会恢复。

重新打开 session 时，历史计划与解读可读，但结果表显示 `Rerun to restore rows`。catalog
snapshot 变化时标记历史为 stale；用户可以查看旧摘要，但重新执行前必须重新确认。

## 与现有功能衔接

### Trajectory Copilot

单 Run 右侧 Copilot 继续服务即时轨迹诊断。Analyze 共享：

- `LlmConfig` 与 Settings；
- OpenAI-compatible HTTP transport；
- provider structured-output / JSON fallback 基础设施；
- markdown/fence 中已有的轨迹证据渲染能力。

Analyze 不共享当前自动执行 `query_sql` 的 tool loop，也不把 analysis revisions 写入
`pchronicle_copilot:<run>` thread。

### Run / Pin / Diff

- Run Detail 增加低干扰 `Analyze this run` 上下文动作；
- root 上下文可进入同 root 范围；
- Pinboard 可选择若干 runs 创建分析；
- Compare 可把左右 runs 作为范围创建分析；
- result identity cell 可回到 Run Detail 或具体 Turn；
- 返回 Analyze 后当前 session/revision 不丢失。

第一阶段不要求 Analyze 直接修改 Pinboard，也不在 interpretation 中自动发起 Diff。

## 错误与边界状态

- **未配置模型：** 保留问题草稿，显示 Settings 引导，不生成假计划。
- **catalog 不可用：** 禁止生成计划，因为模型缺少可靠 schema。
- **plan JSON 无效：** 自动修复一次，失败后显示重试与 provider 原始错误摘要。
- **SQL 被服务端拒绝：** 保留 plan 和编辑内容，错误显示在确认卡下方。
- **查询超时/网络失败：** 可用同一确认 SQL 重试；不自动重试写入新 revision。
- **空结果：** 不请求结论型 interpretation，只说明查询返回 0 行并提供改写问题入口。
- **结果截断：** 表头与 interpretation 同时显示限制，full profile 仍需新计划。
- **解读失败：** 表格和 profiles 正常可用，用户可单独重试 interpretation。
- **scope/catalog 变化：** 未执行计划变 stale；已执行 revision 保留原 snapshot。
- **本地存储失败：** 当前内存会话继续工作并提示不会跨刷新恢复。

## 隐私与安全

- API key 延续现有行为，只在浏览器 `localStorage`，不发送给 pChronicle 服务端。
- schema、用户问题和 evidence digest 由浏览器直接发往用户配置的模型 endpoint。
- 页面在首次配置模型时明确说明将发送的上下文类别。
- SQL 必须经过服务端现有 read-only validator；前端不把模型判断当安全边界。
- 模型生成 SQL、人工编辑 SQL、可视化 refinement SQL 都使用同一确认与查询路径。
- 不渲染模型生成的 HTML/JavaScript；结构化字段按文本处理。

## 前端组件边界

建议将现有单文件 `ToolsWorkspace` 拆为职责清晰的模块：

- `analysis.rs`：workspace 编排、session/revision 状态机；
- `analysis_agent.rs`：plan generation、interpretation、provider fallback；
- `analysis_session.rs`：localStorage、预算、catalog fencing；
- `result_explorer.rs`：结果表、identity links、column profile 交互；
- `result_profile.rs`：纯函数类型推断、统计、bins 与 Top-K；
- `tools.rs`：迁移期可保留路由入口，最终只 re-export workspace；
- `components.rs`：复用通用 `DataTable`/JSON cell，不塞入分析状态机。

模块之间使用结构化 model，不通过 HTML 字符串或自由文本传递 plan/interpretation 状态。
现有单 Run `agent.rs` 只抽取真正通用的 transport/config；不为了复用而合并两种工作流。

## 测试与验收

### 纯逻辑单测

- plan 状态只能 `PlanReady → Executing`，模型返回本身不能触发查询；
- scope 改变使未执行 plan stale；历史执行结果 snapshot 不变；
- plan/interpretation 严格 JSON 解析和一次修复上限；
- number、boolean、categorical、text、datetime、object/list、identifier、mixed 推断；
- histogram 空列、单值、负数、极端值、非有限值；
- Top-K 稳定排序与 `other`；
- truncated/returned/max budget 的 profile scope 文案；
- visual refinement 只产生 intent，不产生 API 调用；
- evidence digest 64 KiB 裁剪顺序与 UTF-8 边界；
- session 20 条/256 KiB 预算、quota 失败和 clear 范围。

### 组件与状态测试

- 未配置 LLM、catalog 失败、plan ready、manual SQL、executing、query error、
  interpreting、interpretation error、complete、stale；
- 高级 SQL 默认折叠，编辑后显示 `Manually edited`；
- 结果表在 interpretation 未完成或失败时仍可探索；
- identity coordinates 完整时生成 deep link，不完整时不生成；
- 空结果不请求 interpretation；
- 恢复 session 后不伪造已恢复 rows。

### 浏览器验收

1. 输入问题后只出现 plan，network 中没有 query 请求。
2. 点击 `Run analysis` 后执行一次只读 query。
3. 结果表显示列 mini profiles；点击列切换详细 profile。
4. 点击 histogram bin 只出现 refinement chip；再次生成并确认后才请求 query。
5. 截断结果同时在表格和 Copilot 解读中标注 preview 限制。
6. SQL 编辑后仍需点击执行，并显示 manual 标记。
7. interpretation 失败时表格不消失。
8. Run/root/Pin/Compare 带入的范围可见且可移除。
9. 刷新后恢复 session 摘要，但结果行要求重新执行。
10. 浏览器控制台没有新错误。

### 构建验收

- `cargo test --manifest-path pchronicle-web/Cargo.toml --locked`
- `cargo fmt --manifest-path pchronicle-web/Cargo.toml -- --check`
- `cargo build --manifest-path pchronicle-web/Cargo.toml --locked`
- `just chronicle-web-build`
- `cargo build --release -p persisting-pchronicle-cli`

不把 AGENTS.md 排除的 Search、Queue/Samplers、TTAS、tiered memory 或
`persisting-dlcapt` 的 workspace-wide 失败纳入本功能验收。
