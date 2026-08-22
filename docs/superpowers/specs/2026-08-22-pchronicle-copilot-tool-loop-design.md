# pChronicle Copilot：浏览器 tool-calling 循环

**日期：** 2026-08-22

**状态：** Approved in conversation

**范围：** `pchronicle-web` 的 Trajectory Copilot（`agent.rs` + `CopilotPanel`）。不改 Warehouse 写路径，不新增服务端 Copilot API。

## 背景

当前 Copilot 是浏览器里的一次性路由器：每条用户问题独立发给 BYOK 模型，模型在 `skill | sql | answer` 里选一次，五个硬编码 skill 大多只扫已加载的 turns/analysis。面板能显示历史，但不会把历史发给模型。Enter 不提交。没有 API key 时仍会跑本地 skill。

目标形态是真正的轨迹助手：能多轮、能主动取证、结论能点回 Trace。推理仍在浏览器 BYOK；密钥不进 pChronicle 服务。

## 目标

1. 用标准 tool-calling 循环替换 `select_action` 路由器和五个 skill。
2. 模型可对**当前打开的 run** 多次调用 `get_analysis`、`get_turn`、`query_sql`，再给出最终回答。
3. 对话按 run 坐标存在 `localStorage`，刷新可恢复。
4. 最终回答用 `[turn:ID]`（以及现有 `pchronicle:trajectory` fence）点回 Trace。
5. 没配 key 时只引导 Settings，不再本地假装分析。
6. 修 Enter 提交（Shift+Enter 换行）。

## 非目标

- 流式正文。
- 服务端 Copilot session、服务端代持密钥。
- 跨 run 对比工具（旧 `cohort_compare` 不迁移）。
- 工作区动作：切 Chats/Steps、Errors only、打开 Analysis tab。
- 旧五个 skill 的兼容层或 `/skill_id` chip。
- Search 子系统、Warehouse 写接口、改 SQL 只读校验。

## 决策

### 循环

用户发送一条消息后：

1. 若 `LlmConfig` 未配齐（api_base / api_key / model），停止并提示打开 Settings。
2. 组装 `messages`：system + 本 run 短分析卡片 + 该 thread 已有 messages + 本轮 user。
3. `POST {api_base}/chat/completions`。
4. **主路径：** 请求带 OpenAI `tools`。若响应含 `tool_calls`，在浏览器执行工具，把 tool 结果 append 进 messages，再请求，直到没有 tool_calls 或达到上限。
5. **回退：** 若提供商拒绝 `tools`、忽略 `tools`、或返回无法解析的 tool 载荷，同一循环改走 JSON：模型只能输出 `{ "tool": "get_analysis"|"get_turn"|"query_sql", "arguments": {} }` 或 `{ "final": "..." }`。连续两轮都不是合法 JSON 则停，并说明该模型接不住 tool-calling。
6. 每条用户消息最多 **8** 次工具调用。用尽仍无最终回答：再请求一次且**不再带 tools**，强制收束。仍空则明确说证据不够。
7. 面板显示当前步骤（例如 `get_turn #12`），不流式打字。

默认模型仍是用户 BYOK 配置；实现必须能在「有 tools」和「无 tools」两种提供商上工作，而不是假设 DeepSeek 支持 `tools`。

### 工具契约

三个工具都钉死在 Copilot 打开时的 `RunSummary` 上。模型参数里不能改 dataset / file / run_id / agent_id / session_id。

| 工具 | 参数 | 执行 | 返回 |
|---|---|---|---|
| `get_analysis` | 无 | 优先用面板已有的 `RunAnalysis`；缺失时 `GET /api/explorer/run` | 指标、source/kind/model breakdown、tool 聚合。不含 turn 全文。 |
| `get_turn` | `turn_id: i64` | `GET /api/explorer/turn` | 摘要 + message / reasoning / tool_calls。正文截到 **8 KiB**，截断落在 UTF-8 边界。 |
| `query_sql` | `sql: string` | `POST /api/query/evidence`，`max_rows=50`，`max_bytes=64KiB` | 现有 `QueryEvidence`。只读约束完全交给服务端（SELECT / WITH / EXPLAIN）。不用 Analyze 页的 100 行 / 4 MiB 预算。 |

发给模型的工具结果是截断后的文本，不是未裁剪的 JSON 瀑布。工具 HTTP/SQL 失败时，把错误字符串当 tool result 交回模型，不中断循环。LLM 传输失败（HTTP、CORS、空响应）停止循环，对话里留一条可重试的错误气泡。

删掉：`trajectory_summary`、`failure_locator`、`latency_hotspots`、`tool_usage`、`cohort_compare`、`resolve_skill`、`select_action`、skill chip、「Include selected turn content once」。

### 系统提示

- 只为当前 run 取证；需要细节就调工具，不要编造。
- 缺测不是零；不能从任意 message 正文推断 error。
- 最终回答用用户语言，3–7 条为宜，事实与推断分开。
- 引用看过的 turn 必须写成 `[turn:ID]`。
- 提及 coverage / truncation（若工具结果带了这些标记）。

短分析卡片（每轮都带、不是 tool 结果）：`session`、`status`、`turn_count`、`event_count`、`error_count`、`total_tokens`、latency P95 与 sample coverage。不塞 turn preview 列表。

若 Trace 里已展开某个 turn，系统提示加一句「用户正在看 turn #N」，**不**自动贴该 turn 正文。要正文必须 `get_turn`。

### 对话存储

键：`pchronicle_copilot:` 加上该 run 的 `RunSummary::query()`（与 explorer URL 同一套坐标，含可选 `run_id` / `root_session_id`）。

值：一份 `CopilotThread`：

- `messages`：user / assistant / tool（含 tool 名、参数摘要、结果文本）
- `updated_at`
- 可选 `truncated`

打开同一条 run 恢复该 thread；换 run 读另一份。不进 pChronicle 服务，不跨浏览器同步。

单条 thread 超过约 **200 KiB**：从最早的 tool 结果砍成短摘要，直到低于上限。发给模型的 messages 总大小若超 **32 KiB**，同样先压缩最早的 tool 内容。数字写死在实现里，不做设置项。

### Trace 衔接

- 最终回答里的 `[turn:ID]`：点击后展开该 turn 并滚进视图（沿用现有 citation 行为，含已落地的 scroll-into-view）。
- `get_turn` 在本轮用户消息中成功过：最终助手消息附带一个 `pchronicle:trajectory` fence，id 为本轮取到的 turn（去重、保序）。聊天里的紧凑 turn 条同样 `on_turn` 跳 Trace。循环中的 tool 结果只显示步骤名，不每步插一份轨迹表。
- Copilot 不修改 Chats/Steps、source 过滤、Errors only、Analysis tab。

### 面板

仍是右侧 overlay。去掉 skill chip。无 key 时展示 Settings 引导。忙碌态显示当前工具名。Enter 提交，Shift+Enter 换行。

### 安全

- 不能通过工具参数切换 run。
- `query_sql` 不在前端再解析 SQL。
- 密钥只在 `localStorage` 的现有 `pchronicle_llm_config`。
- 不新增 Warehouse 写接口。

## 文件

- `pchronicle-web/src/agent.rs` — 循环、工具分发、JSON 回退、线程序列化；删除 skill 路由器
- `pchronicle-web/src/workspace.rs` — `CopilotPanel`：持久化、步骤、Enter、去掉 chip 与 selected-turn checkbox
- `pchronicle-web/src/api.rs` — 沿用 `turn_detail` / `run_analysis` / `query_evidence`；不新开 Copilot 端点
- 现有 citation / `trajectory_fence` / `parse_rich_blocks` 继续用

## 测试

`pchronicle-web` 单测，不打真 LLM：

- 线程键：`pchronicle_copilot:` + `RunSummary::query()`；不同 session 隔离；缺 `run_id` 时键仍稳定
- 200 KiB 裁剪：先砍最早 tool 结果，user/assistant 文本保留
- 原生 `tool_calls` 解析与三次调用后的 messages 形状
- JSON 回退：合法 `{tool}` / `{final}`；连续两轮非法则停
- 未知工具名 → 错误字符串作为 tool result，循环可继续
- 第 8 次之后强制无 tools 收束
- `get_turn` 截断落在 UTF-8 边界（沿用现有 `truncate` 测试）

不要求 e2e。Enter 提交是面板行为，用现有交互约定，不为按键单独上浏览器测试。
