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
│  捕获    请求/响应 → CaptureRecord  │
│  落盘    Lance（canonical）         │
│  物化    -f md 时结束 → Markdown    │
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

### 4.1 分层

```
{存储根}/{agent}/{session}/           ← 主 agent（Lance dataset + 可选 .md）
{存储根}/{agent}/{session}/subagents/{子 session}/
```

- **Agent**：通常对应项目或仓库标识。
- **Session**：一次 capture run 或用户指定的逻辑会话 id。
- **Subagent**：并行子任务，各自独立 Lance dataset。

### 4.2 路由原则

| 场景 | 写入位置 |
|------|----------|
| 主 agent 对话 | 主 session Lance |
| 带 subagent 标识的请求 | 对应 subagent Lance |
| 内部探测（count_tokens 等） | 主 session Lance（materialize 时跳过） |
| subagent 完成通知 | 主 session |

**Markdown 过滤**（materialize 阶段）：主 session 上 flash/haiku companion 探测可跳过；subagent 目录保留更完整对话块。

### 4.3 主 ↔ 子关联

| 机制 | 作用 |
|------|------|
| 流式即时链接 | spawn 工具块完成时主 session 可见提示 |
| 注册匹配 | subagent 首次请求与 spawn 块对齐 |
| 延迟回填 | 时序颠倒时补写 `llm.spawn_link` |
| 工具结果 | subagent 完成时更新引用 |

关联写入 **Lance**；materialize 到 Markdown 时在块元数据与正文脚注中保留 spawn 信息。

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
| `md` | Lance + **流式 append** Markdown |
| `bin` | 仅 Lance |

---

## 6. 运行模式

| 模式 | 适用 |
|------|------|
| **守护进程** | 长期本地开发；多终端共享代理 |
| **前台 serve** | 调试路由与捕获 |
| **capture run** | 一次性：代理 → 子命令 → 退出 → materialize（`-f md`） |

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
| Phase 3 | 流式逐 chunk 记录、转换层测试对齐 agentgateway | 进行中 |
| Phase 4 | Capture 批量 flush 调优、serve 周期 materialize | 进行中 |
| Phase 5 | 多 provider 完整转换、failover / 限流 | 规划 |
| Phase 6 | import 与 serve 按 session 自动去重合并 | 规划 |

---

## 9. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [`persisting capture` 命令](cli_capture_command.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
