# 内嵌 LLM 代理与 Capture

Persisting 内置轻量 **LLM 反向代理**：在转发 OpenAI 兼容 API 的同时，将每次调用的请求与响应写入轨迹。**无需**部署 agentgateway，也无需 OTLP 旁路。

---

## 1. 定位

| 维度 | 说明 |
|------|------|
| **问题** | Agent 开发中 LLM 调用分散在 SDK / IDE / 网关，难以完整留痕 |
| **方案** | 所有 LLM 流量经 Persisting 代理 → 自动捕获 → 落盘为轨迹 |
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
│  对话    请求/响应 → 轨迹事件       │
│  落盘    Lance 或 Markdown         │
└───────────────────────────────────┘
```

- **配置**：YAML 描述 listen、models、upstream、鉴权环境变量。
- **路由**：支持透传（body 不变）与改写（将客户端 model 映射到真实 upstream model）。
- **代理**：OpenAI 兼容路径为主；Anthropic messages 路径可最小转换为 chat completions。
- **对话层**：过滤噪声、渲染人读块、维护 turn 与 token 元数据。
- **落盘**：见 [轨迹存储模型](trajectory_storage.zh.md)。

`capture import` 与 `serve` / `run` **互补**：前者合并 IDE 本地日志或历史网关导出，后者捕获实时 API 流量。

---

## 3. 配置模型（概念）

每条 **model 规则** 要么是：

- **目标定义**：名称 + upstream + 鉴权；或
- **匹配规则**：通配名称 + `forward` 指向某目标定义

省略 `forward` 即透传。规则顺序：**具体优先于通配**。

客户端将 SDK 的 `base_url` 指向代理，并通过 **session 请求头**（可配置）把多次调用归入同一逻辑会话。

---

## 4. 会话与目录拓扑

### 4.1 分层

```
{存储根}/{agent}/{session}/           ← 主 agent
{存储根}/{agent}/{session}/subagents/{子 session}/   ← 并行 subagent
```

- **Agent**：通常对应项目或仓库标识。
- **Session**：一次 capture run 或用户指定的逻辑会话 id。
- **Subagent**：Claude Code 等环境下的并行子任务，各自独立目录。

### 4.2 路由原则

| 场景 | 写入位置 |
|------|----------|
| 主 agent 对话 | 主 session 目录 |
| 带 subagent 标识的请求 | 对应 subagent 子目录 |
| 内部探测流量（如快速的标题生成等轻量探测请求） | 主 session，避免污染 subagent |
| subagent 完成通知 | 主 session（汇总视角） |

**并行 subagent 必须可区分**：同一逻辑 session 下的多个子 agent 靠独立标识分流，否则会错误合并。

**Markdown 过滤**：主 session 中与后续正式轮次重复的 companion 探测请求可跳过写入；subagent 目录内保留完整上下文。

### 4.3 主 ↔ 子关联

主 session 需能回答：「这轮对话 spawn 了哪些 subagent？它们的轨迹在哪？」

关联通过多条互补路径建立：

| 机制 | 作用 |
|------|------|
| 流式即时链接 | Agent 工具块完成时，主 session 即可见 spawn 提示 |
| 注册匹配 | subagent 首次请求与主 session 中 spawn 块按语义对齐 |
| 延迟回填 | 若时序颠倒（assistant 先于 subagent 注册），在 subagent 启动时补写链接 |
| 工具结果 | 主 agent 收到 subagent 完成结果时，更新引用列表 |

元数据挂在 Markdown 块或逻辑事件中，正文末尾也可有显式引用注释供人读。

---

## 5. 捕获内容

每次 LLM 调用至少产生一对事件：

| 类型 | 内容 |
|------|------|
| 请求 | model、路径、解析后的 body |
| 响应 | HTTP 状态、body（流式可先缓冲再落盘） |

来源标记为代理捕获，与 IDE 导入、网关导出可合并为统一时间线。

存储格式：`md`（会话 Markdown，默认）或 `bin`（Lance 列存）。

---

## 6. 运行模式

| 模式 | 适用 |
|------|------|
| **守护进程** | 长期本地开发；多终端共享同一代理 |
| **前台 serve** | 调试路由与捕获逻辑 |
| **capture run** | 一次性：启动代理 → 执行子命令（如 `claude`）→ 退出即停 |

`run` 为子进程注入代理环境变量，HTTPS 隧道与非 LLM 流量透明转发，仅 LLM API 路径被捕获。

管理面提供会话列表、token 用量与估算成本；默认仅本机 admin 端口。

---

## 7. 协议与厂商

按 HTTP 路径识别 API 风格（chat completions、messages、responses、embeddings 等）。多厂商 upstream 通过配置中的 provider 字段区分；完整 Bedrock / Vertex 转换列为后续演进项。

---

## 8. 演进路线

| 阶段 | 内容 | 状态 |
|------|------|------|
| Phase 1 | OpenAI 透传、模型路由、实时落盘 | ✅ |
| Phase 2 | 协议最小转换、subagent 拓扑、spawn 关联、流式 partial 写入 | ✅ |
| Phase 3 | 流式逐 chunk 记录、转换层测试对齐 agentgateway | 进行中 |
| Phase 4 | 多 provider 完整转换、failover / 限流 | 规划 |
| Phase 5 | import 与 serve 按 session 自动去重合并 | 规划 |

---

## 9. 相关文档

- [轨迹存储模型](trajectory_storage.zh.md)
- [`persisting capture` 命令](cli_capture_command.zh.md)
- [轨迹 Markdown 格式](trajectory_tlv_format.zh.md)
