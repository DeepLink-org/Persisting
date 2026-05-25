# 内嵌 LLM 代理与 Capture

Persisting 内置轻量 **LLM 反向代理**：在转发 OpenAI 兼容 API 的同时，将每次调用的请求与响应写入轨迹。**无需**部署 agentgateway，也无需 OTLP 旁路。

---

## 1. 定位

| 维度 | 说明 |
|------|------|
| **问题** | Agent 开发中 LLM 调用分散在 SDK / IDE / 网关，难以完整留痕 |
| **方案** | 所有 LLM 流量经 Persisting 代理 → 自动捕获 → **Lance raw event log** |
| **边界** | 聚焦路由、转发、捕获；不复制 agentgateway 的控制面与全量多厂商策略 |

与 [agentgateway](https://github.com/agentgateway/agentgateway) 的关系：**借鉴配置语义与路由模型**，按需移植转换层，而非硬依赖其运行时。

| 能力 | agentgateway | Persisting 内嵌代理 |
|------|--------------|---------------------|
| 模型名路由（通配 / 前缀） | ✅ | ✅ |
| 多 upstream、API Key | ✅ | ✅ |
| OpenAI 兼容转发 | ✅ | ✅ |
| 请求/响应默认入轨迹 | 需额外遥测配置 | ✅ |
| 全量多厂商转换、MCP、K8s 控制面 | ✅ | 按需演进 |

---

## 2. 组件职责

```
capture serve / run
        │
        ▼
┌───────────────────────────────────┐
│  配置    监听地址、模型路由表        │
│  路由    按 model 名选择 upstream   │
│  代理    HTTP 转发与协议适配        │
│  捕获    CaptureEngine（V3 Actor）→ CaptureRecord  │
│  调度    spawn_capture_apply（非阻塞，不挡 LLM 响应） │
│  落盘    Lance（canonical）+ dead letter / reconcile │
│  物化    -f md 时 live upsert + frontmatter 摘要     │
└───────────────────────────────────┘
```

- **配置**：YAML 描述 listen、models、upstream、鉴权环境变量。
- **路由**：支持透传与 model 映射改写。
- **代理**：OpenAI 兼容路径为主；Anthropic messages 可最小转换为 chat completions。
- **对话层**：定义 materialize 时的过滤规则（skip 内部流量、lifecycle 等）。
- **落盘**：见 [轨迹存储模型](trajectory_storage.zh.md)。

`capture import` 与 `serve` / `run` **互补**：前者合并 IDE 日志或历史导出，后者捕获实时 API 流量。

---

## 3. 配置模型（概念）

每条 **model 规则** 要么是目标定义，要么是通配 + `forward`。省略 `forward` 即透传。规则顺序：**具体优先于通配**。

客户端将 SDK 的 `base_url` 指向代理，并通过 **session 请求头** 把多次调用归入同一逻辑会话。

---

## 4. 会话与目录拓扑

### 4.1 Capture run 布局（当前实现）

一次 `capture run` / 守护进程 run 在 `{存储根}/{agent_id}/run-{timestamp}-{nanos}/` 下**共享同一 run 目录**；主 agent 与各 subagent 的 Lance dataset 与 Markdown **并列存放**，而非嵌套 `subagents/` 子目录：

```
{store}/{agent_id}/run-20260524-161537-122998000/
├── run-20260524-161537-122998000.md     ← 主 agent 人读视图（run bucket）
├── agent-a2560e716f0b8b526.md           ← subagent 1
├── agent-a0ff1c6f08dccbb57.md           ← subagent 2
├── agent-….md                           ← 更多 subagent 并列
└── .lance/
    ├── run-20260524-161537-122998000/   ← lifecycle（session.started 等）
    ├── fb47835b-e10d-4b29-abc3-…/       ← 主 session Lance（header session id）
    └── agent-{subagent_id}/             ← 各 subagent Lance
```

| 概念 | 说明 |
|------|------|
| **Agent** | 配置中的 `agent_id`（如 `deepseek-proxy`），通常对应项目/代理实例 |
| **Run** | 一次 capture 会话目录名（`run-*`），写入 `.capture/run_session` |
| **逻辑 session** | 请求头中的 Claude session id（写入 `CaptureRecord.session_id`） |
| **storage_session_id** | Lance dataset 名与 Markdown **文件名 stem**（路由键） |
| **Subagent** | `x-claude-code-agent-id` 存在时，`storage_session_id = agent-{id}` |

### 4.2 路由原则（Lance）

| 场景 | Lance 写入位置 |
|------|----------------|
| 主 agent 对话 | `.lance/{header_session_id}/` 或 `.lance/{run_id}/`（lifecycle） |
| 带 subagent 标识的请求 | `.lance/agent-{id}/` |
| 内部探测（count_tokens 等） | 主 session Lance（materialize 时跳过） |
| subagent 完成通知（task notification） | 主 session Lance |

### 4.3 Markdown 路径与主/子隔离（`-f md`）

`-f md` 时由 **CaptureEngine** 在 Proxy 进程内 live upsert Markdown（按 `call_id` + `role`）；路径仍由 `session_markdown_write_path_for_key` / `locate_session_markdown_for_key` 解析：

| storage_session_id | 目标 Markdown | fallback 规则 |
|--------------------|---------------|---------------|
| `agent-{id}` | `{run_dir}/agent-{id}.md` | **不**继承 run 主文件；文件不存在则新建 |
| 主 session（header UUID 等） | 优先 `{run_dir}/run-{run_dir名}.md` | capture run 下使用 **run bucket**；**不**扫描 `agent-*.md` |
| 其它 / 扁平 layout | `{run_dir}/{storage_session_id}.md` | 兼容 `0001.md`、`trajectory.tlv.md` |

**隔离 invariant**：

- subagent 的 user/assistant/tool 块 **只** upsert 到对应 `agent-*.md`
- 主 agent 对话与 `llm.spawn_link` **只** upsert 到 run bucket（或 `{header_session}.md`）
- 主 md 通过 `spawn_links` / `<!-- persisting:subagent-refs … -->` 引用 subagent 文件，**不**内联 subagent 全文
- assistant 流式块在 md 中 **原地 rewrite**（draft → final），不向 Lance 写 partial 事件
- 每次 dialogue 写入后刷新 **YAML frontmatter**（turns / tokens / cost / subagents）

**Run 结束后**：Worker flush Lance → 写 `.capture/reconcile.json` 对账 md 与 Lance call_id；stderr 打印 session 摘要。若 `reconcile.json` 报告不一致，再执行 `persisting trajectory materialize` 全量重建 md。

**Materialize 过滤**（有损）：主 session 上 flash/haiku companion 探测可跳过；subagent 文件保留更完整对话块。详见 [轨迹存储模型 §1](trajectory_storage.zh.md)。

### 4.4 主 ↔ 子关联

| 机制 | 作用 |
|------|------|
| 流式即时链接 | spawn 工具块完成时主 session 可见提示 |
| 注册匹配 | subagent 首次请求与 spawn 块对齐 |
| 延迟回填 | 时序颠倒时补写 `llm.spawn_link` |
| 工具结果 | subagent 完成时更新引用 |

关联写入 **Lance**；live upsert 或 materialize 到 Markdown 时，在主块元数据与 `llm.spawn_link` note 中保留 spawn 信息，subagent 块写入 sibling `agent-*.md`。

---

## 5. 捕获内容

每次 LLM 调用至少产生一对 Lance 事件：

| kind | 内容 |
|------|------|
| `llm.request` | model、path、body / summary |
| `llm.response` / `llm.response.stream` | status、body / assistant 文本 |

Session 边界：

| kind | 时机 |
|------|------|
| `session.started` | `capture run` / daemon 启动 |
| `session.ended` | 子进程退出 / 代理关闭 |

存储格式：

| `-f` | 行为 |
|------|------|
| `md` | Lance + Proxy **CaptureEngine live upsert** Markdown（import 仍走批量 append） |
| `bin` | 仅 Lance |

---

## 6. 运行模式

| 模式 | 适用 |
|------|------|
| **守护进程** | 长期本地开发；多终端共享代理 |
| **前台 serve** | 调试路由与捕获 |
| **capture run** | 一次性：代理 → 子命令 → 退出；`-f md` 时 CaptureEngine live upsert；结束可 materialize |

`run` 为子进程注入代理环境变量；非 LLM 流量透明转发。

---

## 7. 协议与厂商

按 HTTP 路径识别 API 风格。多厂商 upstream 通过配置 provider 区分；Bedrock / Vertex 完整转换列为后续演进项。

---

## 8. 演进路线

| 阶段 | 内容 | 状态 |
|------|------|------|
| Phase 1 | OpenAI 透传、模型路由、Lance 落盘 | ✅ |
| Phase 2 | 两层存储、materialize/compact、lifecycle 事件 | ✅ |
| Phase 3 | 流式 SSE 翻译、CaptureEngine draft upsert | ✅ |
| Phase 4 | Capture 批量 flush、主/子 Markdown 路径隔离、live upsert | ✅ |
| Phase 5 | live md 并发可靠性、多 provider 完整转换、failover / 限流 | 规划 |
| Phase 6 | import 与 serve 按 session 自动去重合并 | 规划 |

---

## 9. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [`persisting capture` 命令](cli_capture_command.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
